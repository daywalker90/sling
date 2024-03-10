use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use bitcoin::secp256k1::PublicKey;
use cln_plugin::Plugin;
use cln_rpc::{
    model::responses::ListpeerchannelsChannelsState,
    primitives::{Amount, ShortChannelId},
};

use log::{debug, info};

use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    time::{self, Instant},
};

use crate::{model::*, rpc_cln::*, util::*};

pub async fn refresh_aliasmap(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path;
    let interval;
    {
        let config = plugin.state().config.lock();
        rpc_path = config.rpc_path.clone();
        interval = config.refresh_aliasmap_interval.value;
    }

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

pub async fn refresh_listpeerchannels_loop(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let interval;
    {
        let config = plugin.state().config.lock();
        interval = config.refresh_peers_interval.value;
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

    let now = Instant::now();
    *plugin.state().peer_channels.lock().await = list_peer_channels(&rpc_path)
        .await?
        .channels
        .ok_or(anyhow!("refresh_listpeerchannels: no channels found!"))?
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
    let rpc_path;
    let interval;
    let my_pubkey;
    let sling_dir;
    {
        let config = plugin.state().config.lock();
        rpc_path = config.rpc_path.clone();
        interval = config.refresh_graph_interval.value;
        my_pubkey = config.pubkey;
        sling_dir = config.sling_dir.clone();
    }
    *plugin.state().graph.lock().await = read_graph(&sling_dir).await?;

    loop {
        {
            let now = Instant::now();
            let mut new_graph = BTreeMap::<PublicKey, Vec<DirectedChannelState>>::new();
            let jobs = read_jobs(
                &Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME),
                &plugin,
            )
            .await?;

            let amounts = jobs.values().map(|job| job.amount_msat);
            // * 2 because we set our liquidity beliefs to / 2 anyways
            let min_amount = amounts.clone().min().unwrap_or(1_000) * 2;
            let max_amount = amounts.max().unwrap_or(10_000_000_000);
            let maxppms = jobs.values().map(|job| job.maxppm);
            // let min_maxppm = maxppms.min().unwrap_or(0);
            let max_maxppm = maxppms.max().unwrap_or(3_000);

            let two_w_ago = (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 1_209_600) as u32;

            info!("Getting all channels in gossip...");
            let channels = list_channels(&rpc_path, None, None, None).await?.channels;
            debug!(
                "Got {} public channels after {}ms!",
                channels.len(),
                now.elapsed().as_millis().to_string()
            );
            let local_channels = plugin.state().peer_channels.lock().await;
            debug!(
                "Got {} local channels after {}ms!",
                local_channels.len(),
                now.elapsed().as_millis().to_string()
            );
            let mut private_channel_added_count = 0;
            for chan in local_channels.values() {
                let private = if let Some(pri) = chan.private {
                    pri
                } else {
                    continue;
                };
                let state = if let Some(st) = chan.state {
                    st
                } else {
                    continue;
                };
                let updates = if let Some(upd) = &chan.updates {
                    upd
                } else {
                    continue;
                };
                let local_updates = if let Some(lupd) = &updates.local {
                    lupd
                } else {
                    continue;
                };
                let remote_updates = if let Some(rupd) = &updates.remote {
                    rupd
                } else {
                    continue;
                };
                if private
                    && (state == ListpeerchannelsChannelsState::CHANNELD_NORMAL
                        || state == ListpeerchannelsChannelsState::CHANNELD_AWAITING_SPLICE)
                {
                    new_graph.entry(my_pubkey).or_default().push(
                        DirectedChannelState::from_directed_channel(DirectedChannel {
                            source: my_pubkey,
                            destination: chan.peer_id.unwrap(),
                            short_channel_id: chan.short_channel_id.unwrap(),
                            scid_alias: Some(chan.alias.as_ref().unwrap().local.unwrap()),
                            fee_per_millionth: local_updates.fee_proportional_millionths.unwrap(),
                            base_fee_millisatoshi: Amount::msat(
                                &local_updates.fee_base_msat.unwrap(),
                            ) as u32,
                            htlc_maximum_msat: local_updates.htlc_maximum_msat,
                            htlc_minimum_msat: local_updates.htlc_minimum_msat.unwrap(),
                            amount_msat: chan.total_msat.unwrap(),
                            delay: local_updates.cltv_expiry_delta.unwrap(),
                        }),
                    );
                    new_graph.entry(chan.peer_id.unwrap()).or_default().push(
                        DirectedChannelState::from_directed_channel(DirectedChannel {
                            source: chan.peer_id.unwrap(),
                            destination: my_pubkey,
                            short_channel_id: chan.short_channel_id.unwrap(),
                            scid_alias: Some(chan.alias.as_ref().unwrap().remote.unwrap()),
                            fee_per_millionth: remote_updates.fee_proportional_millionths.unwrap(),
                            base_fee_millisatoshi: Amount::msat(
                                &remote_updates.fee_base_msat.unwrap(),
                            ) as u32,
                            htlc_maximum_msat: remote_updates.htlc_maximum_msat,
                            htlc_minimum_msat: remote_updates.htlc_minimum_msat.unwrap(),
                            amount_msat: chan.total_msat.unwrap(),
                            delay: remote_updates.cltv_expiry_delta.unwrap(),
                        }),
                    );
                    private_channel_added_count += 2;
                }
            }

            info!(
                "Added {} private channels to sling graph after {}ms!",
                private_channel_added_count,
                now.elapsed().as_millis().to_string()
            );

            let mut public_channel_added_count = 0;
            for chan in &channels {
                if (feeppm_effective(
                    chan.fee_per_millionth,
                    chan.base_fee_millisatoshi,
                    max_amount,
                ) as u32
                    <= max_maxppm
                    && Amount::msat(&chan.htlc_maximum_msat.unwrap_or(chan.amount_msat))
                        >= min_amount
                    && Amount::msat(&chan.htlc_minimum_msat) <= max_amount
                    && chan.last_update >= two_w_ago
                    && chan.delay <= 288
                    && chan.active)
                    || chan.source == my_pubkey
                    || chan.destination == my_pubkey
                {
                    new_graph.entry(chan.source).or_default().push(
                        DirectedChannelState::from_directed_channel(DirectedChannel {
                            source: chan.source,
                            destination: chan.destination,
                            short_channel_id: chan.short_channel_id,
                            scid_alias: None,
                            fee_per_millionth: chan.fee_per_millionth,
                            base_fee_millisatoshi: chan.base_fee_millisatoshi,
                            htlc_maximum_msat: chan.htlc_maximum_msat,
                            htlc_minimum_msat: chan.htlc_minimum_msat,
                            amount_msat: chan.amount_msat,
                            delay: chan.delay,
                        }),
                    );
                    public_channel_added_count += 1;
                }
            }
            info!(
                "Added {} public channels to sling graph after {}ms!",
                public_channel_added_count,
                now.elapsed().as_millis().to_string()
            );

            plugin
                .state()
                .graph
                .lock()
                .await
                .update(LnGraph { graph: new_graph });

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
        .value;
    loop {
        {
            let now = Instant::now();
            plugin
                .state()
                .graph
                .lock()
                .await
                .refresh_liquidity(interval);
            info!(
                "Refreshed Liquidity in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(600)).await;
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
                let peer_channels = plugin.state().peer_channels.lock().await;
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
            let stats_delete_successes_age = plugin
                .state()
                .config
                .lock()
                .stats_delete_successes_age
                .value;
            let stats_delete_failures_age =
                plugin.state().config.lock().stats_delete_failures_age.value;
            let stats_delete_successes_size = plugin
                .state()
                .config
                .lock()
                .stats_delete_successes_size
                .value;
            let stats_delete_failures_size = plugin
                .state()
                .config
                .lock()
                .stats_delete_failures_size
                .value;
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
                    .open(sling_dir.join(chan_id.to_string() + SUCCESSES_SUFFIX))
                    .await?;
                file.set_len(0).await?;
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
