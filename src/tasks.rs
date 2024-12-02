use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Error;
use cln_plugin::Plugin;
use cln_rpc::{
    model::{
        requests::{ListnodesRequest, ListpeerchannelsRequest},
        responses::ListpeerchannelsChannelsState,
    },
    primitives::{Amount, ShortChannelId},
    ClnRpc,
};

use log::{debug, info};

use sling::DirectedChannel;
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    time::{self, Instant},
};

use crate::{gossip::read_gossip_store, model::*, util::*};

pub async fn refresh_aliasmap(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path;
    let interval;
    {
        let config = plugin.state().config.lock();
        rpc_path = config.rpc_path.clone();
        interval = config.refresh_aliasmap_interval;
    }
    let mut rpc = ClnRpc::new(&rpc_path).await?;

    loop {
        let now = Instant::now();
        {
            let nodes = rpc.call_typed(&ListnodesRequest { id: None }).await?.nodes;
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

pub async fn refresh_listpeerchannels_loop(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let interval;
    {
        let config = plugin.state().config.lock();
        interval = config.refresh_peers_interval;
    }

    loop {
        {
            refresh_listpeerchannels(&plugin).await?;
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_listpeerchannels(plugin: &Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path;
    {
        let config = plugin.state().config.lock();
        rpc_path = config.rpc_path.clone();
    }
    let mut rpc = ClnRpc::new(&rpc_path).await?;

    let now = Instant::now();
    *plugin.state().peer_channels.lock() = rpc
        .call_typed(&ListpeerchannelsRequest { id: None })
        .await?
        .channels
        .into_iter()
        .filter_map(|channel| channel.short_channel_id.map(|id| (id, channel)))
        .collect();
    debug!(
        "Peerchannels refreshed in {}ms",
        now.elapsed().as_millis().to_string()
    );
    Ok(())
}

pub async fn refresh_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    // let rpc_path;
    let interval;
    let my_pubkey;
    let sling_dir;
    let mut offset = 0;
    {
        let config = plugin.state().config.lock();
        // rpc_path = config.rpc_path.clone();
        interval = config.refresh_gossmap_interval;
        my_pubkey = config.pubkey;
        sling_dir = config.sling_dir.clone();
    }
    *plugin.state().graph.lock() = read_graph(&sling_dir).await?;
    // let mut rpc = ClnRpc::new(&rpc_path).await?;

    loop {
        {
            let now = Instant::now();
            // let jobs = read_jobs(
            //     &Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME),
            //     &plugin,
            // )
            // .await?;

            // let amounts = jobs.values().map(|job| job.amount_msat);
            // * 2 because we set our liquidity beliefs to / 2 anyways
            // let min_amount = amounts.clone().min().unwrap_or(1_000) * 2;
            // let max_amount = amounts.max().unwrap_or(10_000_000_000);
            // let maxppms = jobs.values().map(|job| job.maxppm);
            // let min_maxppm = maxppms.min().unwrap_or(0);
            // let max_maxppm = maxppms.max().unwrap_or(3_000);

            // let two_w_ago = (SystemTime::now()
            //     .duration_since(UNIX_EPOCH)
            //     .unwrap()
            //     .as_secs()
            //     - 1_209_600) as u32;
            {
                debug!("Getting all channels in gossip_store...");
                read_gossip_store(plugin.clone(), &mut offset).await?;
                debug!(
                    "Reading gossip store done after {}ms!",
                    now.elapsed().as_millis().to_string()
                );

                let mut lngraph = plugin.state().graph.lock();
                info!(
                    "{} public channels in sling graph after {}ms!",
                    lngraph
                        .graph
                        .values()
                        .flatten()
                        .filter(|(_, y)| y.scid_alias.is_none())
                        .count(),
                    now.elapsed().as_millis().to_string()
                );

                let local_channels = plugin.state().peer_channels.lock().clone();
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                debug!(
                    "Got {} local channels after {}ms!",
                    local_channels.len(),
                    now.elapsed().as_millis().to_string()
                );
                for chan in local_channels.values() {
                    let private = if let Some(pri) = chan.private {
                        pri
                    } else {
                        continue;
                    };
                    if !private {
                        continue;
                    }
                    let updates = if let Some(upd) = &chan.updates {
                        upd
                    } else {
                        // pre 24.02 versions don't have this and
                        // get private channel gossip from gossip_store
                        // but we don't get the alias there
                        if let Some(dir_chans) = lngraph.graph.get_mut(&my_pubkey) {
                            if let Some(dir_chan_state) = dir_chans.get_mut(&DirectedChannel {
                                short_channel_id: chan.short_channel_id.unwrap(),
                                direction: chan.direction.unwrap(),
                            }) {
                                dir_chan_state.scid_alias =
                                    Some(chan.alias.as_ref().unwrap().local.unwrap())
                            }
                        }
                        if let Some(dir_chans) = lngraph.graph.get_mut(&chan.peer_id) {
                            if let Some(dir_chan_state) = dir_chans.get_mut(&DirectedChannel {
                                short_channel_id: chan.short_channel_id.unwrap(),
                                direction: chan.direction.unwrap() ^ 1,
                            }) {
                                dir_chan_state.scid_alias =
                                    Some(chan.alias.as_ref().unwrap().remote.unwrap())
                            }
                        }
                        continue;
                    };
                    let remote_updates = if let Some(rupd) = &updates.remote {
                        rupd
                    } else {
                        continue;
                    };
                    if chan.state == ListpeerchannelsChannelsState::CHANNELD_NORMAL
                        || chan.state == ListpeerchannelsChannelsState::CHANNELD_AWAITING_SPLICE
                    {
                        lngraph.graph.entry(my_pubkey).or_default().insert(
                            DirectedChannel {
                                short_channel_id: chan.short_channel_id.unwrap(),
                                direction: chan.direction.unwrap(),
                            },
                            DirectedChannelState {
                                source: my_pubkey,
                                destination: chan.peer_id,
                                scid_alias: Some(chan.alias.as_ref().unwrap().local.unwrap()),
                                fee_per_millionth: updates.local.fee_proportional_millionths,
                                base_fee_millisatoshi: Amount::msat(&updates.local.fee_base_msat)
                                    as u32,
                                htlc_maximum_msat: updates.local.htlc_maximum_msat,
                                htlc_minimum_msat: updates.local.htlc_minimum_msat,
                                amount_msat: chan.total_msat.unwrap(),
                                delay: updates.local.cltv_expiry_delta,
                                active: chan.peer_connected,
                                last_update: timestamp as u32,
                                liquidity: chan.spendable_msat.unwrap().msat(),
                                liquidity_age: timestamp,
                            },
                        );
                        lngraph.graph.entry(chan.peer_id).or_default().insert(
                            DirectedChannel {
                                short_channel_id: chan.short_channel_id.unwrap(),
                                direction: chan.direction.unwrap() ^ 1,
                            },
                            DirectedChannelState {
                                source: chan.peer_id,
                                destination: my_pubkey,
                                scid_alias: Some(chan.alias.as_ref().unwrap().remote.unwrap()),
                                fee_per_millionth: remote_updates.fee_proportional_millionths,
                                base_fee_millisatoshi: Amount::msat(&remote_updates.fee_base_msat)
                                    as u32,
                                htlc_maximum_msat: remote_updates.htlc_maximum_msat,
                                htlc_minimum_msat: remote_updates.htlc_minimum_msat,
                                amount_msat: chan.total_msat.unwrap(),
                                delay: remote_updates.cltv_expiry_delta,
                                active: chan.peer_connected,
                                last_update: timestamp as u32,
                                liquidity: chan.receivable_msat.unwrap().msat(),
                                liquidity_age: timestamp,
                            },
                        );
                    }
                }
                for channels in lngraph.graph.values_mut() {
                    channels.retain(|k, v| {
                        v.scid_alias.is_none()
                            || v.scid_alias.is_some()
                                && if let Some(c) = local_channels.get(&k.short_channel_id) {
                                    c.state == ListpeerchannelsChannelsState::CHANNELD_NORMAL ||
                                c.state == ListpeerchannelsChannelsState::CHANNELD_AWAITING_SPLICE
                                } else {
                                    false
                                }
                    })
                }

                info!(
                    "{} private channels in sling graph after {}ms!",
                    lngraph
                        .graph
                        .values()
                        .flatten()
                        .filter(|(_, y)| y.scid_alias.is_some())
                        .count(),
                    now.elapsed().as_millis().to_string()
                );

                lngraph.graph.retain(|_, v| !v.is_empty());
            }
            // match write_graph(plugin.clone()).await {
            //     Ok(_) => (),
            //     Err(e) => return Err(anyhow!("Too dumb to write....{}", e)),
            // };
            info!(
                "Refreshed graph in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        {
            let interval = plugin.state().config.lock().reset_liquidity_interval;
            let now = Instant::now();
            plugin.state().graph.lock().refresh_liquidity(interval);
            info!(
                "Refreshed Liquidity in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(120)).await;
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
            let mut all_jobs: Vec<ShortChannelId> =
                pull_jobs.into_iter().chain(push_jobs.into_iter()).collect();

            let scid_peer_map;
            {
                let peer_channels = plugin.state().peer_channels.lock();
                scid_peer_map = get_all_normal_channels_from_listpeerchannels(&peer_channels);
            }

            all_jobs.retain(|c| scid_peer_map.contains_key(c));
            for scid in &all_jobs {
                match SuccessReb::read_from_file(&sling_dir, scid).await {
                    Ok(o) => {
                        successes.insert(scid, o);
                    }
                    Err(e) => debug!("{}: probably no success stats yet: {:?}", scid, e),
                };

                match FailureReb::read_from_file(&sling_dir, scid).await {
                    Ok(o) => {
                        failures.insert(scid, o);
                    }
                    Err(e) => debug!("{}: probably no failure stats yet: {:?}", scid, e),
                };
            }
            let stats_delete_successes_age =
                plugin.state().config.lock().stats_delete_successes_age;
            let stats_delete_failures_age = plugin.state().config.lock().stats_delete_failures_age;
            let stats_delete_successes_size =
                plugin.state().config.lock().stats_delete_successes_size;
            let stats_delete_failures_size =
                plugin.state().config.lock().stats_delete_failures_size;
            let sys_time_now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let succ_age = sys_time_now - stats_delete_successes_age * 24 * 60 * 60;
            let fail_age = sys_time_now - stats_delete_failures_age * 24 * 60 * 60;
            for (chan_id, rebs) in successes {
                let rebs_len = rebs.len();
                let filtered_rebs = if stats_delete_successes_age > 0 {
                    rebs.into_iter()
                        .filter(|c| c.completed_at >= succ_age)
                        .collect::<Vec<SuccessReb>>()
                } else {
                    rebs
                };
                let filtered_rebs_len = filtered_rebs.len();
                debug!(
                    "{}: filtered {} success entries because of age",
                    chan_id,
                    rebs_len - filtered_rebs_len
                );
                let pruned_rebs = if stats_delete_successes_size > 0
                    && filtered_rebs_len as u64 > stats_delete_successes_size
                {
                    filtered_rebs
                        .into_iter()
                        .skip(filtered_rebs_len - stats_delete_successes_size as usize)
                        .collect::<Vec<SuccessReb>>()
                } else {
                    filtered_rebs
                };
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
                    .truncate(true)
                    .open(sling_dir.join(chan_id.to_string() + SUCCESSES_SUFFIX))
                    .await?;
                file.write_all(&content).await?;
            }
            for (chan_id, rebs) in failures {
                let rebs_len = rebs.len();
                let filtered_rebs = if stats_delete_failures_age > 0 {
                    rebs.into_iter()
                        .filter(|c| c.created_at >= fail_age)
                        .collect::<Vec<FailureReb>>()
                } else {
                    rebs
                };
                let filtered_rebs_len = filtered_rebs.len();
                debug!(
                    "{}: filtered {} failure entries because of age",
                    chan_id,
                    rebs_len - filtered_rebs_len
                );
                let pruned_rebs = if stats_delete_failures_size > 0
                    && filtered_rebs_len as u64 > stats_delete_failures_size
                {
                    filtered_rebs
                        .into_iter()
                        .skip(filtered_rebs_len - stats_delete_failures_size as usize)
                        .collect::<Vec<FailureReb>>()
                } else {
                    filtered_rebs
                };
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
                    .truncate(true)
                    .open(sling_dir.join(chan_id.to_string() + FAILURES_SUFFIX))
                    .await?;
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
