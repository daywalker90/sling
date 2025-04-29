use anyhow::{anyhow, Error};

use cln_plugin::Plugin;

use cln_rpc::model::requests::SendpayRoute;
use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::*;

use sling::{DirectedChannel, Job, SatDirection};
use std::cmp::{max, min};

use std::collections::HashMap;

use tokio::time::Instant;

use crate::dijkstra::dijkstra;
use crate::model::{
    Config, DijkstraNode, ExcludeGraph, JobMessage, PluginState, PublicKeyPair, Task,
};
use crate::response::{sendpay_response, waitsendpay_response};
use crate::util::{
    feeppm_effective, feeppm_effective_from_amts, get_normal_channel_from_listpeerchannels,
    get_preimage_paymend_hash_pair, get_total_htlc_count, is_channel_normal, my_sleep,
};
use crate::{channel_jobstate_update, get_remote_feeppm_effective, wait_for_gossip, LnGraph};

pub async fn sling(job: &Job, task: &Task, plugin: &Plugin<PluginState>) -> Result<(), Error> {
    wait_for_gossip(plugin, task).await?;

    let mut success_route: Option<Vec<SendpayRoute>> = None;
    'outer: loop {
        let now = Instant::now();
        let should_stop = plugin
            .state()
            .job_state
            .lock()
            .get(&task.chan_id)
            .unwrap()
            .iter()
            .find(|jt| jt.id() == task.task_id)
            .unwrap()
            .should_stop();
        if should_stop {
            log::info!("{}/{}: Stopped job!", task.chan_id, task.task_id);
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                task,
                &JobMessage::Stopped,
                false,
                true,
            )?;
            break;
        }

        let config = plugin.state().config.lock().clone();

        let tempbans = plugin.state().tempbans.lock().clone();
        let peer_channels = plugin.state().peer_channels.lock().clone();
        let other_peer = peer_channels
            .get(&task.chan_id)
            .ok_or(anyhow!("other_peer: channel not found"))?
            .peer_id;

        if let Some(r) = health_check(
            plugin,
            &config,
            &peer_channels,
            task,
            job,
            other_peer,
            &tempbans,
        )
        .await?
        {
            if r {
                continue 'outer;
            }
            break 'outer;
        }

        channel_jobstate_update(
            plugin.state().job_state.clone(),
            task,
            &JobMessage::Rebalancing,
            true,
            false,
        )?;

        let route = {
            let nr = next_route(
                plugin,
                &config,
                &peer_channels,
                job,
                &tempbans,
                task,
                &PublicKeyPair {
                    my_pubkey: config.pubkey,
                    other_pubkey: other_peer,
                },
                &mut success_route,
            )
            .await;
            if nr.is_err() || nr.as_ref().unwrap().is_empty() {
                log::info!(
                    "{}/{}: could not find a route. Sleeping...",
                    task.chan_id,
                    task.task_id
                );
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    task,
                    &JobMessage::NoRoute,
                    true,
                    false,
                )?;
                success_route = None;
                my_sleep(600, plugin.state().job_state.clone(), task).await;
                continue 'outer;
            }
            nr.unwrap()
        };

        let fee_ppm_effective = feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        );
        log::info!(
            "{}/{}: Found {}ppm route with {} hops. Total: {}ms",
            task.chan_id,
            task.task_id,
            fee_ppm_effective,
            route.len() - 1,
            now.elapsed().as_millis().to_string()
        );

        if fee_ppm_effective > job.maxppm {
            log::info!(
                "{}/{}: route not cheap enough! Sleeping...",
                task.chan_id,
                task.task_id
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                task,
                &JobMessage::TooExp,
                true,
                false,
            )?;
            my_sleep(600, plugin.state().job_state.clone(), task).await;
            success_route = None;
            continue 'outer;
        }

        {
            let alias_map = plugin.state().alias_peer_map.lock();
            for r in &route {
                log::debug!(
                    "{}/{}: route: {} {:4} {:17} {}",
                    task.chan_id,
                    task.task_id,
                    Amount::msat(&r.amount_msat),
                    r.delay,
                    r.channel.to_string(),
                    alias_map.get(&r.id).unwrap_or(&r.id.to_string()),
                );
            }
        }

        let (preimage, payment_hash) = get_preimage_paymend_hash_pair();
        // debug!(
        //     "{}: Made preimage and payment_hash: {} Total: {}ms",
        //     chan_id.to_string(),
        //     payment_hash.to_string(),
        //     now.elapsed().as_millis().to_string()
        // );

        let send_response = match sendpay_response(
            plugin,
            &config,
            payment_hash,
            preimage,
            task,
            job,
            &route,
            &mut success_route,
        )
        .await
        {
            Ok(o) => {
                if let Some(resp) = o {
                    resp
                } else {
                    continue;
                }
            }
            Err(e) => {
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    task,
                    &JobMessage::Error,
                    false,
                    true,
                )?;
                log::warn!("{}", e);
                break 'outer;
            }
        };
        log::info!(
            "{}/{}: Sent on route. Total: {}ms",
            task.chan_id,
            task.task_id,
            now.elapsed().as_millis().to_string()
        );

        match waitsendpay_response(
            plugin,
            &config,
            send_response.payment_hash,
            task,
            now,
            job,
            &route,
            &mut success_route,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    task,
                    &JobMessage::Error,
                    false,
                    true,
                )?;
                log::warn!("{}", e);
                break 'outer;
            }
        };
    }
    if let Some(tk) = plugin.state().parrallel_bans.lock().get_mut(&task.chan_id) {
        tk.remove(&task.task_id);
    };

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn next_route(
    plugin: &Plugin<PluginState>,
    config: &Config,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    job: &Job,
    tempbans: &HashMap<ShortChannelId, u64>,
    task: &Task,
    keypair: &PublicKeyPair,
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<Vec<SendpayRoute>, Error> {
    let graph = plugin.state().graph.lock();
    #[allow(clippy::clone_on_copy)]
    let blockheight = plugin.state().blockheight.lock().clone();
    let candidatelist;
    if let Some(c) = &job.candidatelist {
        if !c.is_empty() {
            candidatelist = build_candidatelist(
                peer_channels,
                job,
                &graph,
                tempbans,
                config,
                Some(c),
                blockheight,
            )
        } else {
            candidatelist = build_candidatelist(
                peer_channels,
                job,
                &graph,
                tempbans,
                config,
                None,
                blockheight,
            )
        }
    } else {
        candidatelist = build_candidatelist(
            peer_channels,
            job,
            &graph,
            tempbans,
            config,
            None,
            blockheight,
        )
    }

    log::debug!(
        "{}/{}: Candidates: {}",
        task.chan_id,
        task.task_id,
        candidatelist
            .iter()
            .map(|y| y.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    );
    if !tempbans.is_empty() {
        log::debug!(
            "{}/{}: Tempbans: {}",
            task.chan_id,
            task.task_id,
            plugin
                .state()
                .tempbans
                .lock()
                .clone()
                .keys()
                .map(|y| y.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    if candidatelist.is_empty() {
        log::info!(
            "{}/{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
            task.chan_id,
            task.task_id
        );
        channel_jobstate_update(
            plugin.state().job_state.clone(),
            task,
            &JobMessage::NoCandidates,
            true,
            false,
        )?;
        return Err(anyhow!("No candidates found"));
    }

    let mut route = Vec::new();
    if let Some(prev_route) = success_route {
        if match job.sat_direction {
            SatDirection::Pull => candidatelist.iter().any(|c| {
                c == &prev_route.first().unwrap().channel
                    || peer_channels
                        .get(c)
                        .and_then(|chan| {
                            chan.alias.as_ref().and_then(|alias| {
                                alias.local.map(|local_alias| {
                                    local_alias == prev_route.first().unwrap().channel
                                })
                            })
                        })
                        .unwrap_or(false)
            }),
            SatDirection::Push => candidatelist.iter().any(|c| {
                c == &prev_route.last().unwrap().channel
                    || peer_channels
                        .get(c)
                        .and_then(|chan| {
                            chan.alias.as_ref().and_then(|alias| {
                                alias.remote.map(|remote_alias| {
                                    remote_alias == prev_route.last().unwrap().channel
                                })
                            })
                        })
                        .unwrap_or(false)
            }),
        } {
            route.clone_from(prev_route);
        } else {
            *success_route = None;
        }
    }

    let mut parallel_bans = plugin.state().parrallel_bans.lock();
    // for (i, ban) in parallel_bans.entry(task.chan_id).or_default().iter() {
    //     debug!(
    //         "{}/{}: {}: Before bans: {}/{}",
    //         task.chan_id, task.task_id, i, ban.short_channel_id, ban.direction
    //     );
    // }
    let task_bans = if let Some(tk) = parallel_bans.get(&task.chan_id) {
        tk.values().cloned().collect()
    } else {
        Vec::new()
    };
    match success_route {
        Some(_) => (),
        None => {
            if let Some(tk) = parallel_bans.get_mut(&task.chan_id) {
                tk.remove(&task.task_id);
            };
            let mut pull_jobs = plugin.state().pull_jobs.lock().clone();
            let mut push_jobs = plugin.state().push_jobs.lock().clone();
            let excepts = plugin.state().excepts_chans.lock().clone();
            let excepts_peers = plugin.state().excepts_peers.lock().clone();
            for except in &excepts {
                pull_jobs.insert(*except);
                push_jobs.insert(*except);
            }
            let max_hops = match job.maxhops {
                Some(h) => h + 1,
                None => config.maxhops + 1,
            };
            match job.sat_direction {
                SatDirection::Pull => {
                    let slingchan_inc =
                        match graph.get_channel(&keypair.other_pubkey, &task.chan_id) {
                            Ok(in_chan) => in_chan,
                            Err(_) => {
                                log::warn!(
                                    "{}/{}: channel not found in graph!",
                                    task.chan_id,
                                    task.task_id
                                );
                                channel_jobstate_update(
                                    plugin.state().job_state.clone(),
                                    task,
                                    &JobMessage::ChanNotInGraph,
                                    true,
                                    false,
                                )?;
                                return Err(anyhow!("channel not found in graph"));
                            }
                        };
                    route = dijkstra(
                        &keypair.my_pubkey,
                        &graph,
                        &keypair.my_pubkey,
                        &keypair.other_pubkey,
                        &DijkstraNode {
                            score: 0,
                            destination: keypair.my_pubkey,
                            channel_state: slingchan_inc,
                            hops: 0,
                            short_channel_id: task.chan_id,
                        },
                        job,
                        &candidatelist,
                        max_hops,
                        &ExcludeGraph {
                            exclude_chans: pull_jobs,
                            exclude_peers: excepts_peers,
                        },
                        config.cltv_delta,
                        tempbans,
                        &task_bans,
                    )?;
                }
                SatDirection::Push => {
                    let slingchan_out = match graph.get_channel(&keypair.my_pubkey, &task.chan_id) {
                        Ok(out_chan) => out_chan,
                        Err(_) => {
                            log::warn!(
                                "{}/{}: channel not found in graph!",
                                task.chan_id,
                                task.task_id
                            );
                            channel_jobstate_update(
                                plugin.state().job_state.clone(),
                                task,
                                &JobMessage::ChanNotInGraph,
                                true,
                                false,
                            )?;
                            return Err(anyhow!("channel not found in graph!"));
                        }
                    };
                    route = dijkstra(
                        &keypair.my_pubkey,
                        &graph,
                        &keypair.other_pubkey,
                        &keypair.my_pubkey,
                        &DijkstraNode {
                            score: 0,
                            destination: keypair.other_pubkey,
                            channel_state: slingchan_out,
                            hops: 0,
                            short_channel_id: task.chan_id,
                        },
                        job,
                        &candidatelist,
                        max_hops,
                        &ExcludeGraph {
                            exclude_chans: push_jobs,
                            exclude_peers: excepts_peers,
                        },
                        config.cltv_delta,
                        tempbans,
                        &task_bans,
                    )?;
                }
            }
        }
    }
    if route.len() >= 3 {
        let route_claim_chan = route[route.len() / 2].channel;
        let route_claim_peer = route[(route.len() / 2) - 1].id;
        if let Some(source_node) = graph.graph.get(&route_claim_peer) {
            let dir_chan_0 = DirectedChannel {
                short_channel_id: route_claim_chan,
                direction: 0,
            };
            let dir_chan_1 = DirectedChannel {
                short_channel_id: route_claim_chan,
                direction: 1,
            };
            for dir_chan in [dir_chan_0, dir_chan_1] {
                if let Some(dir_chan_state_0) = source_node.get(&dir_chan) {
                    if dir_chan_state_0.source != config.pubkey
                        && dir_chan_state_0.destination != config.pubkey
                    {
                        parallel_bans
                            .entry(task.chan_id)
                            .or_default()
                            .insert(task.task_id, dir_chan);
                    } else if let Some(tk) = parallel_bans.get_mut(&task.chan_id) {
                        tk.remove(&task.task_id);
                    }
                }
            }
        };
        // for (i, ban) in parallel_bans.entry(task.chan_id).or_default().iter() {
        //     debug!(
        //         "{}/{}: {}: After bans: {}/{}",
        //         task.chan_id, task.task_id, i, ban.short_channel_id, ban.direction
        //     );
        // }
    } else if let Some(tk) = parallel_bans.get_mut(&task.chan_id) {
        tk.remove(&task.task_id);
    };
    Ok(route)
}

async fn health_check(
    plugin: &Plugin<PluginState>,
    config: &Config,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    task: &Task,
    job: &Job,
    other_peer: PublicKey,
    tempbans: &HashMap<ShortChannelId, u64>,
) -> Result<Option<bool>, Error> {
    let job_states = plugin.state().job_state.clone();

    let channel = match get_normal_channel_from_listpeerchannels(peer_channels, &task.chan_id) {
        Ok(o) => o,
        Err(e) => {
            log::warn!("{}/{}: {}. Stopping Job.", task.chan_id, task.task_id, e);
            channel_jobstate_update(
                job_states.clone(),
                task,
                &JobMessage::ChanNotNormal,
                false,
                true,
            )?;
            return Ok(Some(false));
        }
    };

    if !check_private_alias(plugin, &channel, config, task, job, other_peer) {
        log::info!(
            "{}/{}: private channel without alias. Taking a break...",
            task.chan_id,
            task.task_id
        );
        channel_jobstate_update(job_states.clone(), task, &JobMessage::NoAlias, true, false)?;
        my_sleep(600, job_states.clone(), task).await;
        return Ok(Some(true));
    }

    if job.is_balanced(&channel, &task.chan_id)
        || match job.sat_direction {
            SatDirection::Pull => Amount::msat(&channel.receivable_msat.unwrap()) < job.amount_msat,
            SatDirection::Push => Amount::msat(&channel.spendable_msat.unwrap()) < job.amount_msat,
        }
    {
        log::info!(
            "{}/{}: already balanced. Taking a break...",
            task.chan_id,
            task.task_id
        );
        channel_jobstate_update(job_states.clone(), task, &JobMessage::Balanced, true, false)?;
        my_sleep(600, job_states.clone(), task).await;
        Ok(Some(true))
    } else if get_total_htlc_count(&channel) > config.max_htlc_count {
        log::info!(
            "{}/{}: already more than {} pending htlcs. Taking a break...",
            task.chan_id,
            task.task_id,
            config.max_htlc_count
        );
        channel_jobstate_update(
            job_states.clone(),
            task,
            &JobMessage::HTLCcapped,
            true,
            false,
        )?;
        my_sleep(10, job_states.clone(), task).await;
        Ok(Some(true))
    } else {
        match peer_channels.values().find(|x| x.peer_id == other_peer) {
            Some(p) => {
                if !p.peer_connected {
                    log::info!(
                        "{}/{}: not connected. Taking a break...",
                        task.chan_id,
                        task.task_id
                    );
                    channel_jobstate_update(
                        job_states.clone(),
                        task,
                        &JobMessage::Disconnected,
                        true,
                        false,
                    )?;
                    my_sleep(60, job_states.clone(), task).await;
                    Ok(Some(true))
                } else if tempbans.contains_key(&task.chan_id) {
                    log::info!(
                        "{}/{}: Job peer not ready. Taking a break...",
                        task.chan_id,
                        task.task_id
                    );
                    channel_jobstate_update(
                        job_states.clone(),
                        task,
                        &JobMessage::PeerNotReady,
                        true,
                        false,
                    )?;
                    my_sleep(20, job_states.clone(), task).await;
                    Ok(Some(true))
                } else {
                    Ok(None)
                }
            }
            None => {
                channel_jobstate_update(
                    job_states.clone(),
                    task,
                    &JobMessage::PeerNotFound,
                    false,
                    true,
                )?;
                log::warn!(
                    "{}/{}: peer not found. Stopping job.",
                    task.chan_id,
                    task.task_id
                );
                Ok(Some(false))
            }
        }
    }
}

fn check_private_alias(
    plugin: &Plugin<PluginState>,
    channel: &ListpeerchannelsChannels,
    config: &Config,
    task: &Task,
    job: &Job,
    other_peer: PublicKey,
) -> bool {
    let graph = plugin.state().graph.lock();
    if let Some(private) = channel.private {
        if private
            && graph
                .get_channel(
                    match job.sat_direction {
                        SatDirection::Pull => &other_peer,
                        SatDirection::Push => &config.pubkey,
                    },
                    &task.chan_id,
                )
                .unwrap()
                .scid_alias
                .is_none()
        {
            return false;
        }
    }
    true
}

fn build_candidatelist(
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    job: &Job,
    graph: &LnGraph,
    tempbans: &HashMap<ShortChannelId, u64>,
    config: &Config,
    custom_candidates: Option<&Vec<ShortChannelId>>,
    blockheight: u32,
) -> Vec<ShortChannelId> {
    let mut candidatelist = Vec::<ShortChannelId>::new();

    let depleteuptopercent = match job.depleteuptopercent {
        Some(dp) => dp,
        None => config.depleteuptopercent,
    };
    let depleteuptoamount = match job.depleteuptoamount {
        Some(dp) => dp,
        None => config.depleteuptoamount,
    };

    for channel in peer_channels.values() {
        if let Some(scid) = channel.short_channel_id {
            if is_channel_normal(channel).is_ok()
                && channel.peer_connected
                && match custom_candidates {
                    Some(c) => c.iter().any(|c| *c == scid),
                    None => true,
                }
                && scid.block() <= blockheight - config.candidates_min_age
            {
                let chan_in_ppm = match get_remote_feeppm_effective(
                    channel,
                    graph,
                    scid,
                    job.amount_msat,
                    &config.version,
                ) {
                    Ok(o) => o,
                    Err(_) => continue,
                };

                let to_us_msat = Amount::msat(&channel.to_us_msat.unwrap());
                let total_msat = Amount::msat(&channel.total_msat.unwrap());
                let chan_out_ppm = feeppm_effective(
                    channel.fee_proportional_millionths.unwrap(),
                    Amount::msat(&channel.fee_base_msat.unwrap()) as u32,
                    job.amount_msat,
                );

                if match job.sat_direction {
                    SatDirection::Pull => {
                        to_us_msat
                            > max(
                                job.amount_msat + 10_000_000,
                                min(
                                    (depleteuptopercent * total_msat as f64) as u64,
                                    depleteuptoamount,
                                ),
                            )
                            && match job.outppm {
                                Some(out) => chan_out_ppm <= out,
                                None => true,
                            }
                    }
                    SatDirection::Push => {
                        total_msat - to_us_msat
                            > max(
                                job.amount_msat + 10_000_000,
                                min(
                                    (depleteuptopercent * total_msat as f64) as u64,
                                    depleteuptoamount,
                                ),
                            )
                            && match job.outppm {
                                Some(out) => chan_out_ppm >= out,
                                None => true,
                            }
                            && job.maxppm as u64 >= chan_in_ppm
                    }
                } && !tempbans.contains_key(&scid)
                    && get_total_htlc_count(channel) <= config.max_htlc_count
                {
                    candidatelist.push(scid);
                }
            }
        }
    }

    candidatelist
}
