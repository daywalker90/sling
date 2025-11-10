use cln_rpc::{model::requests::GetinfoRequest, ClnRpc};
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::{path::Path, time::Duration};

use anyhow::anyhow;
use cln_plugin::{
    options::{
        ConfigOption,
        DefaultBooleanConfigOption,
        DefaultIntegerConfigOption,
        DefaultStringArrayConfigOption,
        DefaultStringConfigOption,
    },
    Builder,
    RpcMethodBuilder,
};
use config::*;
use htlc::{block_added, htlc_handler};
use model::*;
use notifications::*;
use rpc_sling::*;
use serde_json::json;
use stats::*;
use tokio::{self, time};
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

const OPT_REFRESH_ALIASMAP_INTERVAL: &str = "sling-refresh-aliasmap-interval";
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
const OPT_AUTOGO: DefaultBooleanConfigOption = ConfigOption::new_bool_with_default(
    "sling-autogo",
    false,
    "Automatically start all jobs on startup. Default is `false`",
);

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    std::env::set_var(
        "CLN_PLUGIN_LOG",
        "cln_plugin=info,cln_rpc=info,sling=trace,debug",
    );
    log_panics::init();
    let state;
    let confplugin;
    let opt_refresh_aliasmap_interval: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_REFRESH_ALIASMAP_INTERVAL,
            3600,
            "Refresh interval for aliasmap task. Default is `3600`",
        )
        .dynamic();
    let opt_reset_liquidity_interval: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_RESET_LIQUIDITY_INTERVAL,
            360,
            "Refresh interval for liquidity reset task. Default is `360`",
        )
        .dynamic();
    let opt_depleteuptopercent: DefaultStringConfigOption = ConfigOption::new_str_with_default(
        OPT_DEPLETEUPTOPERCENT,
        "0.2",
        "Deplete up to percent for candidate search. Default is `0.2`",
    )
    .dynamic();
    let opt_depleteuptoamount: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_DEPLETEUPTOAMOUNT,
        2000000000,
        "Deplete up to amount for candidate search. Default is `2000000000`",
    )
    .dynamic();
    let opt_maxhops: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_MAXHOPS,
        8,
        "Maximum number of hops in a route. Default is `8`",
    )
    .dynamic();
    let opt_candidates_min_age: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_CANDIDATES_MIN_AGE,
        0,
        "Minium age of a candidate to rebalance with in days. Default is `0`",
    )
    .dynamic();
    let opt_paralleljobs: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_PARALLELJOBS,
        1,
        "Number of parallel tasks for a job. Default is `1`",
    )
    .dynamic();
    let opt_timeoutpay: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_TIMEOUTPAY,
        120,
        "Timeout for rebalances until we give up and continue. Default is `120`",
    )
    .dynamic();
    let opt_max_htlc_count: DefaultIntegerConfigOption = ConfigOption::new_i64_with_default(
        OPT_MAX_HTLC_COUNT,
        5,
        "Max number of htlc allowed pending in job and candidate. Default is `5`",
    )
    .dynamic();
    let opt_stats_delete_failures_age: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_STATS_DELETE_FAILURES_AGE,
            30,
            "Max age of failure stats in days. Default is `30`",
        )
        .dynamic();
    let opt_stats_delete_failures_size: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_STATS_DELETE_FAILURES_SIZE,
            10000,
            "Max number of failure stats per channel. Default is `10000`",
        )
        .dynamic();
    let opt_stats_delete_successes_age: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_STATS_DELETE_SUCCESSES_AGE,
            30,
            "Max age of success stats in days. Default is `30`",
        )
        .dynamic();
    let opt_stats_delete_successes_size: DefaultIntegerConfigOption =
        ConfigOption::new_i64_with_default(
            OPT_STATS_DELETE_SUCCESSES_SIZE,
            10000,
            "Max number of success stats per channel. Default is `10000`",
        )
        .dynamic();
    let opt_inform_layers: DefaultStringArrayConfigOption = ConfigOption::new_str_arr_with_default(
        OPT_INFORM_LAYERS,
        "xpay",
        "Inform these layers about our information we gather from rebalances. \
         Can be stated multiple times",
    );
    match Builder::new(tokio::io::stdin(), tokio::io::stdout())
        .hook("htlc_accepted", htlc_handler)
        .subscribe("block_added", block_added)
        .option(opt_refresh_aliasmap_interval)
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
        .option(OPT_AUTOGO)
        .setconfig_callback(setconfig_callback)
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-job"), slingjob)
                .description("Add a sling job")
                .usage(
                    "-k scid direction amount maxppm [outppm] [target] [maxhops] \
                [candidates] [depleteuptopercent] [depleteuptoamount] [paralleljobs]",
                ),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(
                &(PLUGIN_NAME.to_string() + "-jobsettings"),
                slingjobsettings,
            )
            .description("Show settings of sling job(s)")
            .usage("[scid]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-deletejob"), slingdeletejob)
                .description("Delete sling job(s)")
                .usage("job [delete_stats]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-go"), slinggo)
                .description("Start sling job(s)")
                .usage("[scid]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-stop"), slingstop)
                .description("Stop sling job(s)")
                .usage("[scid]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-once"), slingonce)
                .description("Run sling rebalance once with fixed amount")
                .usage(
                    "-k scid direction amount maxppm onceamount [outppm] [maxhops] \
        [candidates] [depleteuptopercent] [depleteuptoamount] [paralleljobs]",
                ),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-stats"), slingstats)
                .description("Show stats on channel(s)")
                .usage("[scid] [json]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-except-chan"), slingexceptchan)
                .description("Manage channels to avoid for all jobs")
                .usage("command [scid]"),
        )
        .rpcmethod_from_builder(
            RpcMethodBuilder::new(&(PLUGIN_NAME.to_string() + "-except-peer"), slingexceptpeer)
                .description("Manage peers to avoid for all jobs")
                .usage("command [id]"),
        )
        .rpcmethod(
            &(PLUGIN_NAME.to_string() + "-version"),
            "Print sling plugin version",
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
            let mut rpc = match ClnRpc::new(&rpc_path).await {
                Ok(o) => o,
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
            };
            let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
            let getinfo = match rpc.call_typed(&GetinfoRequest {}).await {
                Ok(o) => o,
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
            };
            let except_chans = match read_except_chans(&sling_dir).await {
                Ok(o) => o,
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
            };
            let except_peers = match read_except_peers(&sling_dir).await {
                Ok(o) => o
                    .into_iter()
                    .map(|p| PubKeyBytes::from_pubkey(&p))
                    .collect(),
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
            };
            let liquidity = match read_liquidity(&sling_dir).await {
                Ok(o) => o,
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
            };
            let config = Config::new(
                getinfo.clone(),
                rpc_path,
                sling_dir,
                except_chans.clone(),
                except_chans,
                except_peers,
            );
            state = PluginState::new(config, liquidity);
            {
                *state.blockheight.lock() = getinfo.blockheight;
            }
            match get_startup_options(&plugin, state.clone()).await {
                Ok(()) => &(),
                Err(e) => return plugin.disable(format!("{e}").as_str()).await,
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
            match tasks::refresh_listpeerchannels_loop(peersclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_listpeers thread: {e:?}"),
            };
            let _res = peersclone.shutdown();
        });
        let channelsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_graph(channelsclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_graph thread: {e:?}"),
            };
            let _res = channelsclone.shutdown();
        });
        let aliasclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_aliasmap(aliasclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_aliasmap thread: {e:?}"),
            };
            let _res = aliasclone.shutdown();
        });
        let liquidityclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::refresh_liquidity(liquidityclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in refresh_liquidity thread: {e:?}"),
            };
            let _res = liquidityclone.shutdown();
        });
        let tempbanclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_tempbans(tempbanclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in clear_tempbans thread: {e:?}"),
            };
            let _res = tempbanclone.shutdown();
        });
        let clearstatsclone = plugin.clone();
        tokio::spawn(async move {
            match tasks::clear_stats(clearstatsclone.clone()).await {
                Ok(()) => (),
                Err(e) => log::warn!("Error in clear_stats thread: {e:?}"),
            };
            let _res = clearstatsclone.shutdown();
        });
        if plugin.state().config.lock().at_or_above_24_11 {
            let askrene_clone = plugin.clone();
            tokio::spawn(async move {
                match tasks::read_askrene_liquidity(askrene_clone.clone()).await {
                    Ok(()) => (),
                    Err(e) => log::warn!("Error in read_askrene_liquidity thread: {e:?}"),
                };
                let _res = askrene_clone.shutdown();
            });
        }

        if plugin.option(&OPT_AUTOGO).unwrap() {
            time::sleep(Duration::from_secs(5)).await;
            let _go_res = slinggo(plugin.clone(), json!({})).await;
        }

        plugin.join().await?;
        std::process::exit(0);
    } else {
        Err(anyhow!("Error starting the plugin!"))
    }
}
