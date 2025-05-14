use std::{
    collections::hash_map,
    fs::File,
    io::BufReader,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Error};
use cln_plugin::Plugin;
use cln_rpc::{
    model::requests::{AskrenelistlayersRequest, ListnodesRequest, ListpeerchannelsRequest},
    primitives::{Amount, ChannelState, ShortChannelIdDir},
    ClnRpc,
};

use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    time::{self, Instant},
};

use crate::{gossip::read_gossip_store, model::*};

pub async fn refresh_aliasmap(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        let now = Instant::now();
        let rpc_path;
        let interval;
        {
            let config = plugin.state().config.lock();
            rpc_path = config.rpc_path.clone();
            interval = config.refresh_aliasmap_interval;
        }
        let mut rpc = ClnRpc::new(&rpc_path).await?;
        {
            let nodes = rpc.call_typed(&ListnodesRequest { id: None }).await?.nodes;
            *plugin.state().alias_peer_map.lock() = nodes
                .into_iter()
                .filter_map(|node| node.alias.map(|alias| (node.nodeid, alias)))
                .collect();
        }
        log::info!(
            "Refreshing alias map done in {}ms!",
            now.elapsed().as_millis()
        );
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_listpeerchannels_loop(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        let interval;
        {
            let config = plugin.state().config.lock();
            interval = config.refresh_peers_interval;
        }
        {
            refresh_listpeerchannels(plugin.clone()).await?;
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_listpeerchannels(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path;
    {
        let config = plugin.state().config.lock();
        rpc_path = config.rpc_path.clone();
    }
    let mut rpc = ClnRpc::new(&rpc_path).await?;

    let now = Instant::now();
    *plugin.state().peer_channels.lock() = rpc
        .call_typed(&ListpeerchannelsRequest {
            id: None,
            short_channel_id: None,
        })
        .await?
        .channels
        .into_iter()
        .filter_map(|channel| channel.short_channel_id.map(|id| (id, channel)))
        .collect();
    log::trace!("Peerchannels refreshed in {}ms", now.elapsed().as_millis());
    Ok(())
}

pub async fn refresh_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let my_pubkey = plugin.state().config.lock().pubkey_bytes;
    let mut is_startup = true;

    let gossip_file =
        File::open(Path::new(&plugin.configuration().lightning_dir).join("gossip_store"))?;

    let mut reader = BufReader::new(gossip_file);

    loop {
        let interval;
        {
            let now = Instant::now();
            {
                let config = plugin.state().config.lock();
                interval = config.refresh_gossmap_interval;
            }
            {
                log::debug!("Getting all channels in gossip_store...");
                read_gossip_store(plugin.clone(), &mut reader, &mut is_startup).await?;
                log::debug!(
                    "Reading gossip store done after {}ms!",
                    now.elapsed().as_millis()
                );

                let mut lngraph = plugin.state().graph.lock();
                log::debug!(
                    "{} public channels in sling graph after {}ms!",
                    lngraph.public_channel_count(),
                    now.elapsed().as_millis()
                );

                let local_channels = plugin.state().peer_channels.lock().clone();
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                log::debug!(
                    "Got {} local channels after {}ms!",
                    local_channels.len(),
                    now.elapsed().as_millis()
                );
                for chan in local_channels.values() {
                    let private = if let Some(pri) = chan.private {
                        pri
                    } else {
                        log::debug!("{:?} is missing private field", chan.short_channel_id);
                        continue;
                    };
                    if !private {
                        continue;
                    }
                    if !(chan.state == ChannelState::CHANNELD_NORMAL
                        || chan.state == ChannelState::CHANNELD_AWAITING_SPLICE)
                    {
                        continue;
                    }
                    let local_alias = chan.alias.as_ref().and_then(|l| l.local);
                    let remote_alias = chan.alias.as_ref().and_then(|l| l.remote);
                    let updates = if let Some(upd) = &chan.updates {
                        upd
                    } else {
                        log::debug!(
                            "{} is missing updates field",
                            chan.short_channel_id.unwrap()
                        );
                        continue;
                    };

                    lngraph.insert(
                        ShortChannelIdDir {
                            short_channel_id: chan.short_channel_id.unwrap(),
                            direction: chan.direction.unwrap(),
                        },
                        ShortChannelIdDirState {
                            source: my_pubkey,
                            destination: PubKeyBytes::from_pubkey(&chan.peer_id),
                            scid_alias: local_alias,
                            fee_per_millionth: updates.local.fee_proportional_millionths,
                            base_fee_millisatoshi: Amount::msat(&updates.local.fee_base_msat)
                                as u32,
                            htlc_maximum_msat: updates.local.htlc_maximum_msat,
                            htlc_minimum_msat: updates.local.htlc_minimum_msat,
                            delay: updates.local.cltv_expiry_delta,
                            active: chan.peer_connected,
                            last_update: timestamp as u32,
                            private: true,
                        },
                    );

                    if let Some(remote_updates) = &updates.remote {
                        lngraph.insert(
                            ShortChannelIdDir {
                                short_channel_id: chan.short_channel_id.unwrap(),
                                direction: chan.direction.unwrap() ^ 1,
                            },
                            ShortChannelIdDirState {
                                source: PubKeyBytes::from_pubkey(&chan.peer_id),
                                destination: my_pubkey,
                                scid_alias: remote_alias,
                                fee_per_millionth: remote_updates.fee_proportional_millionths,
                                base_fee_millisatoshi: Amount::msat(&remote_updates.fee_base_msat)
                                    as u32,
                                htlc_maximum_msat: remote_updates.htlc_maximum_msat,
                                htlc_minimum_msat: remote_updates.htlc_minimum_msat,
                                delay: remote_updates.cltv_expiry_delta,
                                active: chan.peer_connected,
                                last_update: timestamp as u32,
                                private: true,
                            },
                        );
                    } else {
                        log::debug!(
                            "No remote gossip found for private channel {}, \
                            not adding that direction to graph",
                            chan.short_channel_id.unwrap()
                        );
                    }
                }

                lngraph.retain(|k, v| {
                    !v.private
                        || if let Some(c) = local_channels.get(&k.short_channel_id) {
                            c.state == ChannelState::CHANNELD_NORMAL
                                || c.state == ChannelState::CHANNELD_AWAITING_SPLICE
                        } else {
                            false
                        }
                });

                log::debug!(
                    "{} private channels in sling graph after {}ms!",
                    lngraph.private_channel_count(),
                    now.elapsed().as_millis()
                );
            }
            log::debug!("Refreshed graph in {}ms!", now.elapsed().as_millis());
        }
        time::sleep(Duration::from_secs(interval)).await;
    }
}

pub async fn refresh_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        {
            let interval = plugin.state().config.lock().reset_liquidity_interval;
            let now = Instant::now();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let mut liquidity = plugin.state().liquidity.lock();
            liquidity.retain(|_, v| v.liquidity_age > timestamp - interval * 60);
            log::info!(
                "Refreshed {} liquidity beliefs in {}ms!",
                liquidity.len(),
                now.elapsed().as_millis()
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
            let successes = SuccessReb::read_from_files(&sling_dir, None).await?;
            let failures = FailureReb::read_from_files(&sling_dir, None).await?;

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
                log::debug!(
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
                log::debug!(
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
                    .open(sling_dir.join(chan_id.to_string() + "_" + SUCCESSES_SUFFIX))
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
                log::debug!(
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
                log::debug!(
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
                    .open(sling_dir.join(chan_id.to_string() + "_" + FAILURES_SUFFIX))
                    .await?;
                file.write_all(&content).await?;
            }
            log::debug!("Pruned stats successfully in {}s!", now.elapsed().as_secs());
        }
        time::sleep(Duration::from_secs(21_600)).await;
    }
}

pub async fn read_askrene_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = plugin.state().config.lock().rpc_path.clone();
    let interval = plugin.state().config.lock().reset_liquidity_interval * 60;
    let mut rpc = ClnRpc::new(&rpc_path).await?;
    loop {
        {
            let now = Instant::now();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let xpay_layer_resp = rpc
                .call_typed(&AskrenelistlayersRequest {
                    layer: Some("xpay".to_owned()),
                })
                .await?;

            let xpay_layer = xpay_layer_resp
                .layers
                .first()
                .ok_or_else(|| anyhow!("no xpay layer"))?;
            let mut liquidity = plugin.state().liquidity.lock();
            let mut counter = 0;
            for belief in xpay_layer.constraints.iter() {
                let scid_dir = if let Some(sd) = belief.short_channel_id_dir {
                    sd
                } else {
                    continue;
                };
                let belief_timestamp = if let Some(ts) = belief.timestamp {
                    ts
                } else {
                    continue;
                };
                let belief_maximum_msat = if let Some(mm) = belief.maximum_msat {
                    mm.msat()
                } else {
                    continue;
                };
                if belief_timestamp < timestamp - interval {
                    continue;
                }
                counter += 1;
                match liquidity.entry(scid_dir) {
                    hash_map::Entry::Occupied(mut occupied_entry) => {
                        if occupied_entry.get().liquidity_age < belief_timestamp {
                            *occupied_entry.get_mut() = Liquidity {
                                liquidity_msat: belief_maximum_msat,
                                liquidity_age: belief_timestamp,
                            };
                        }
                    }
                    hash_map::Entry::Vacant(vacant_entry) => {
                        vacant_entry.insert(Liquidity {
                            liquidity_msat: belief_maximum_msat,
                            liquidity_age: belief_timestamp,
                        });
                    }
                };
            }
            log::info!(
                "Read {} askerene liquidity constraints in {}ms!",
                counter,
                now.elapsed().as_millis()
            );
        }
        time::sleep(Duration::from_secs(60)).await;
    }
}
