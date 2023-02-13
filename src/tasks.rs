use std::{
    collections::HashMap,
    path::Path,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    list_channels, list_nodes, list_peers,
    model::{
        DirectedChannel, FailureReb, LnGraph, PluginState, SuccessReb, FAILURES_SUFFIX,
        SUCCESSES_SUFFIX,
    },
    util::{
        get_all_normal_channels_from_listpeers, make_rpc_path, read_graph, read_jobs,
        refresh_joblists, write_graph,
    },
    PLUGIN_NAME,
};
use anyhow::Error;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId};

use log::{debug, info};

use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    time::{self, Instant},
};

pub async fn refresh_aliasmap(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);
    let interval = plugin
        .state()
        .config
        .lock()
        .clone()
        .refresh_aliasmap_interval
        .1;

    loop {
        let now = Instant::now();
        {
            let nodes = list_nodes(&rpc_path, None).await?.nodes;
            *plugin.state().alias_peer_map.lock() = nodes
                .into_iter()
                .filter_map(|node| node.alias.map(|alias| (node.nodeid, alias)))
                .collect();
        }
        info!(
            "Refreshing alias map done in {}ms!",
            now.elapsed().as_millis().to_string()
        );
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_listpeers(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);
    let interval = plugin
        .state()
        .config
        .lock()
        .clone()
        .refresh_peers_interval
        .1;

    loop {
        {
            let now = Instant::now();
            *plugin.state().peers.lock() = list_peers(&rpc_path).await?.peers;
            debug!(
                "Peers refreshed in {}ms",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    *plugin.state().graph.lock() = read_graph(&sling_dir).await?;
    let interval = plugin
        .state()
        .config
        .lock()
        .clone()
        .refresh_graph_interval
        .1;

    loop {
        {
            let now = Instant::now();
            info!("Getting all channels in gossip...");
            let channels = list_channels(&rpc_path, None, None, None).await?.channels;
            info!(
                "Getting all channels done in {}s!",
                now.elapsed().as_secs().to_string()
            );
            let peers = plugin.state().peers.lock().clone();
            let jobs = read_jobs(
                &Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME),
                &peers,
            )
            .await?;

            let amounts = jobs.values().map(|job| job.amount);
            let min_amount = amounts.clone().min().unwrap_or(1_000);
            let max_amount = amounts.max().unwrap_or(10_000_000_000);
            let maxppms = jobs.values().map(|job| job.maxppm);
            // let min_maxppm = maxppms.min().unwrap_or(0);
            let max_maxppm = maxppms.max().unwrap_or(3_000);

            let mypubkey = plugin.state().config.lock().pubkey.unwrap().clone();

            let two_w_ago = (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 1_209_600) as u32;

            plugin.state().graph.lock().update(LnGraph {
                graph: channels
                    .into_iter()
                    .filter(|chan| {
                        (chan.active
                            && chan.last_update >= two_w_ago
                            && chan.delay <= 288
                            && Amount::msat(&chan.htlc_maximum_msat.unwrap_or(chan.amount_msat))
                                >= min_amount
                            && Amount::msat(&chan.htlc_minimum_msat) <= max_amount
                            && chan.fee_per_millionth <= max_maxppm)
                            || chan.source == mypubkey
                            || chan.destination == mypubkey
                    })
                    .fold(HashMap::new(), |mut map, chan| {
                        map.entry(chan.source)
                            .or_insert_with(Vec::new)
                            .push(DirectedChannel::new(chan));
                        map
                    }),
            });

            write_graph(plugin.clone()).await?;
            info!(
                "Built and saved graph in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let interval = plugin
        .state()
        .config
        .lock()
        .clone()
        .reset_liquidity_interval
        .1;
    loop {
        {
            let now = Instant::now();
            plugin.state().graph.lock().refresh_liquidity(interval);
            info!(
                "Refreshed Liquidity in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(900)).await;
    }
}

pub async fn clear_tempbans(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        {
            plugin.state().tempbans.lock().retain(|_c, t| {
                *t > SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    - 600
            })
        }
        time::sleep(Duration::from_secs(100)).await;
    }
}

pub async fn clear_stats(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    loop {
        {
            let now = Instant::now();
            let mut successes = HashMap::new();
            let mut failures = HashMap::new();
            refresh_joblists(plugin.clone()).await?;
            let pull_jobs = plugin.state().pull_jobs.lock().clone();
            let push_jobs = plugin.state().push_jobs.lock().clone();
            let mut all_jobs: Vec<String> =
                pull_jobs.into_iter().chain(push_jobs.into_iter()).collect();
            let peers = plugin.state().peers.lock().clone();
            let scid_peer_map = get_all_normal_channels_from_listpeers(&peers);
            all_jobs.retain(|c| scid_peer_map.contains_key(c));
            for scid in &all_jobs {
                match SuccessReb::read_from_file(
                    &sling_dir,
                    ShortChannelId::from_str(&scid.clone())?,
                )
                .await
                {
                    Ok(o) => {
                        successes.insert(scid, o);
                    }
                    Err(e) => debug!("{}: probably no success stats yet: {:?}", scid, e),
                };

                match FailureReb::read_from_file(
                    &sling_dir,
                    ShortChannelId::from_str(&scid.clone())?,
                )
                .await
                {
                    Ok(o) => {
                        failures.insert(scid, o);
                    }
                    Err(e) => debug!("{}: probably no failure stats yet: {:?}", scid, e),
                };
            }
            let stats_delete_successes_age =
                plugin.state().config.lock().stats_delete_successes_age.1;
            let stats_delete_failures_age =
                plugin.state().config.lock().stats_delete_failures_age.1;
            let stats_delete_successes_size =
                plugin.state().config.lock().stats_delete_successes_size.1;
            let stats_delete_failures_size =
                plugin.state().config.lock().stats_delete_failures_size.1;
            let sys_time_now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let succ_age = sys_time_now - stats_delete_successes_age * 24 * 60 * 60;
            let fail_age = sys_time_now - stats_delete_failures_age * 24 * 60 * 60;
            for (chan_id, rebs) in successes {
                let rebs_len = rebs.len();
                let filtered_rebs;
                if stats_delete_successes_age > 0 {
                    filtered_rebs = rebs
                        .into_iter()
                        .filter(|c| c.completed_at >= succ_age)
                        .collect::<Vec<SuccessReb>>();
                } else {
                    filtered_rebs = rebs;
                }
                let filtered_rebs_len = filtered_rebs.len();
                debug!(
                    "{}: filtered {} success entries because of age",
                    chan_id,
                    rebs_len - filtered_rebs_len
                );
                let pruned_rebs;
                if stats_delete_successes_size > 0
                    && filtered_rebs_len as u64 > stats_delete_successes_size
                {
                    pruned_rebs = filtered_rebs
                        .into_iter()
                        .skip(filtered_rebs_len - stats_delete_successes_size as usize)
                        .collect::<Vec<SuccessReb>>();
                } else {
                    pruned_rebs = filtered_rebs;
                }
                debug!(
                    "{}: filtered {} success entries because of size",
                    chan_id,
                    filtered_rebs_len - pruned_rebs.len()
                );
                let mut content: Vec<u8> = vec![];
                for reb in &pruned_rebs {
                    let serialized = serde_json::to_string(&reb)?;
                    content.extend(format!("{}\n", serialized).as_bytes());
                }

                let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(sling_dir.join(chan_id.to_string() + SUCCESSES_SUFFIX))
                    .await?;
                file.set_len(0).await?;
                file.write_all(&content).await?;
            }
            for (chan_id, rebs) in failures {
                let rebs_len = rebs.len();
                let filtered_rebs;
                if stats_delete_failures_age > 0 {
                    filtered_rebs = rebs
                        .into_iter()
                        .filter(|c| c.created_at >= fail_age)
                        .collect::<Vec<FailureReb>>();
                } else {
                    filtered_rebs = rebs;
                }
                let filtered_rebs_len = filtered_rebs.len();
                debug!(
                    "{}: filtered {} failure entries because of age",
                    chan_id,
                    rebs_len - filtered_rebs_len
                );
                let pruned_rebs;
                if stats_delete_failures_size > 0
                    && filtered_rebs_len as u64 > stats_delete_failures_size
                {
                    pruned_rebs = filtered_rebs
                        .into_iter()
                        .skip(filtered_rebs_len - stats_delete_failures_size as usize)
                        .collect::<Vec<FailureReb>>();
                } else {
                    pruned_rebs = filtered_rebs;
                }
                debug!(
                    "{}: filtered {} failure entries because of size",
                    chan_id,
                    filtered_rebs_len - pruned_rebs.len()
                );
                let mut content: Vec<u8> = vec![];
                for reb in &pruned_rebs {
                    let serialized = serde_json::to_string(&reb)?;
                    content.extend(format!("{}\n", serialized).as_bytes());
                }

                let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(sling_dir.join(chan_id.to_string() + FAILURES_SUFFIX))
                    .await?;
                file.set_len(0).await?;
                file.write_all(&content).await?;
            }
            debug!(
                "Pruned stats successfully in {}s!",
                now.elapsed().as_secs().to_string()
            );
        }
        time::sleep(Duration::from_secs(21_600)).await;
    }
}
