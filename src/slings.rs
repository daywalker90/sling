use anyhow::{anyhow, Error};

use cln_plugin::Plugin;

use cln_rpc::model::requests::SendpayRoute;
use cln_rpc::model::responses::{ListpeerchannelsChannels, ListpeerchannelsChannelsState};
use cln_rpc::primitives::*;

use log::{debug, info, warn};

use sling::{Job, SatDirection};
use std::cmp::{max, min};
use std::collections::BTreeMap;
use std::path::Path;

use std::time::SystemTime;
use std::{collections::HashMap, time::UNIX_EPOCH};
use std::{path::PathBuf, str::FromStr};

use tokio::time::Instant;

use crate::dijkstra::dijkstra;
use crate::errors::WaitsendpayErrorData;
use crate::model::{
    Config, DijkstraNode, ExcludeGraph, FailureReb, JobMessage, PluginState, PublicKeyPair,
    SuccessReb, Task, PLUGIN_NAME,
};
use crate::rpc::{slingsend, waitsendpay2};
use crate::util::{
    channel_jobstate_update, feeppm_effective, feeppm_effective_from_amts,
    get_normal_channel_from_listpeerchannels, get_preimage_paymend_hash_pair, get_total_htlc_count,
    is_channel_normal, my_sleep,
};

pub async fn sling(
    rpc_path: &PathBuf,
    job: &Job,
    task: &Task,
    plugin: &Plugin<PluginState>,
) -> Result<(), Error> {
    let config = plugin.state().config.lock().clone();
    let mypubkey = config.pubkey.unwrap();

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let mut networkdir = PathBuf::from_str(&plugin.configuration().lightning_dir).unwrap();
    networkdir.pop();

    loop {
        {
            let graph = plugin.state().graph.lock().await;

            if graph.graph.is_empty() {
                info!(
                    "{}/{}: graph is still empty. Sleeping...",
                    task.chan_id, task.task_id
                );
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    task,
                    &JobMessage::GraphEmpty,
                    None,
                    None,
                );
            } else {
                break;
            }
        }
        my_sleep(600, plugin.state().job_state.clone(), task).await;
    }
    let other_peer;
    {
        let peer_channels = plugin.state().peer_channels.lock().await;

        other_peer = peer_channels
            .get(&task.chan_id)
            .ok_or(anyhow!("other_peer: channel not found"))?
            .peer_id
            .ok_or(anyhow!("other_peer: peer id gone"))?;
    }

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
            info!("{}/{}: Stopped job!", task.chan_id, task.task_id);
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                task,
                &JobMessage::Stopped,
                Some(false),
                None,
            );
            break;
        }

        let tempbans = plugin.state().tempbans.lock().clone();
        let peer_channels = plugin.state().peer_channels.lock().await.clone();

        if let Some(r) = health_check(
            plugin.clone(),
            &peer_channels,
            task,
            job,
            other_peer,
            &tempbans,
        )
        .await
        {
            if r {
                continue 'outer;
            } else {
                break 'outer;
            }
        }

        channel_jobstate_update(
            plugin.state().job_state.clone(),
            task,
            &JobMessage::Rebalancing,
            None,
            None,
        );

        let route = match next_route(
            plugin,
            &peer_channels,
            job,
            &tempbans,
            task,
            &PublicKeyPair {
                my_pubkey: mypubkey,
                other_pubkey: other_peer,
            },
            &mut success_route,
        )
        .await
        {
            Ok(r) => r,
            Err(_e) => {
                success_route = None;
                my_sleep(600, plugin.state().job_state.clone(), task).await;
                continue 'outer;
            }
        };

        if route.is_empty() {
            info!(
                "{}/{}: could not find a route. Sleeping...",
                task.chan_id, task.task_id
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                task,
                &JobMessage::NoRoute,
                None,
                None,
            );
            success_route = None;
            my_sleep(600, plugin.state().job_state.clone(), task).await;
            continue 'outer;
        }

        let fee_ppm_effective = feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        );
        info!(
            "{}/{}: Found {}ppm route with {} hops. Total: {}ms",
            task.chan_id,
            task.task_id,
            fee_ppm_effective,
            route.len() - 1,
            now.elapsed().as_millis().to_string()
        );

        if fee_ppm_effective > job.maxppm {
            info!(
                "{}/{}: route not cheap enough! Sleeping...",
                task.chan_id, task.task_id
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                task,
                &JobMessage::TooExp,
                None,
                None,
            );
            my_sleep(600, plugin.state().job_state.clone(), task).await;
            success_route = None;
            continue 'outer;
        }

        {
            let alias_map = plugin.state().alias_peer_map.lock();
            for r in &route {
                debug!(
                    "{}/{}: route: {} {:4} {:17} {}",
                    task.chan_id,
                    task.task_id,
                    Amount::msat(&r.amount_msat),
                    r.delay,
                    r.channel,
                    alias_map.get(&r.id).unwrap_or(&r.id.to_string()),
                );
            }
        }

        let route_claim_chan = route[route.len() / 2].channel;
        let route_claim_peer = route[(route.len() / 2) - 1].id;
        debug!(
            "{}/{}: setting liquidity on {} to 0 to not get same route for parallel tasks.",
            task.chan_id, task.task_id, route_claim_chan
        );
        let route_claim_liq = match plugin
            .state()
            .graph
            .lock()
            .await
            .graph
            .get_mut(&route_claim_peer)
            .unwrap()
            .iter_mut()
            .find_map(|x| {
                if x.channel.short_channel_id == route_claim_chan
                    && x.channel.destination != mypubkey
                    && x.channel.source != mypubkey
                {
                    x.liquidity = 0;
                    Some(x)
                } else {
                    None
                }
            }) {
            Some(dc) => dc.liquidity,
            None => 0,
        };

        let (preimage, payment_hash) = get_preimage_paymend_hash_pair();
        // debug!(
        //     "{}: Made preimage and payment_hash: {} Total: {}ms",
        //     chan_id.to_string(),
        //     payment_hash.to_string(),
        //     now.elapsed().as_millis().to_string()
        // );

        let send_response;
        match slingsend(rpc_path, route.clone(), payment_hash, None, None).await {
            Ok(resp) => {
                plugin
                    .state()
                    .pays
                    .write()
                    .insert(payment_hash.to_string(), preimage);
                send_response = resp;
            }
            Err(e) => {
                if e.to_string().contains("First peer not ready") {
                    info!(
                        "{}/{}: First peer not ready, banning it for now...",
                        task.chan_id, task.task_id
                    );
                    plugin.state().tempbans.lock().insert(
                        route.first().unwrap().channel,
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    );
                    success_route = None;
                    FailureReb {
                        amount_msat: job.amount_msat,
                        failure_reason: "FIRST_PEER_NOT_READY".to_string(),
                        failure_node: route.first().unwrap().id,
                        channel_partner: match job.sat_direction {
                            SatDirection::Pull => route.first().unwrap().channel,
                            SatDirection::Push => route.last().unwrap().channel,
                        },
                        hops: (route.len() - 1) as u8,
                        created_at: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    }
                    .write_to_file(task.chan_id, &sling_dir)
                    .await?;
                    continue;
                } else {
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        task,
                        &JobMessage::Error,
                        None,
                        Some(false),
                    );
                    warn!(
                        "{}/{}: Unexpected sendpay error: {}",
                        task.chan_id,
                        task.task_id,
                        e.to_string()
                    );
                    break 'outer;
                }
            }
        };
        info!(
            "{}/{}: Sent on route. Total: {}ms",
            task.chan_id,
            task.task_id,
            now.elapsed().as_millis().to_string()
        );

        let err_chan = match waitsendpay2(
            rpc_path,
            send_response.payment_hash,
            config.timeoutpay.value,
        )
        .await
        {
            Ok(o) => {
                info!(
                    "{}/{}: Rebalance SUCCESSFULL after {}s. Sent {}sats plus {}msats fee",
                    task.chan_id,
                    task.task_id,
                    now.elapsed().as_secs().to_string(),
                    Amount::msat(&o.amount_msat.unwrap()) / 1_000,
                    Amount::msat(&o.amount_sent_msat) - Amount::msat(&o.amount_msat.unwrap()),
                );

                SuccessReb {
                    amount_msat: Amount::msat(&o.amount_msat.unwrap()),
                    fee_ppm: feeppm_effective_from_amts(
                        Amount::msat(&o.amount_sent_msat),
                        Amount::msat(&o.amount_msat.unwrap()),
                    ),
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel,
                        SatDirection::Push => route.last().unwrap().channel,
                    },
                    hops: (route.len() - 1) as u8,
                    completed_at: o.completed_at.unwrap() as u64,
                }
                .write_to_file(task.chan_id, &sling_dir)
                .await?;
                success_route = Some(route);
                None
            }
            Err(err) => {
                success_route = None;
                plugin
                    .state()
                    .pays
                    .write()
                    .remove(&send_response.payment_hash.to_string());
                let mut special_stop = false;
                match &err.downcast() {
                    Ok(RpcError {
                        code,
                        message,
                        data,
                    }) => {
                        let ws_code = if let Some(c) = code {
                            c
                        } else {
                            channel_jobstate_update(
                                plugin.state().job_state.clone(),
                                task,
                                &JobMessage::Error,
                                None,
                                Some(false),
                            );
                            warn!(
                                "{}/{}: No WaitsendpayErrorCode, instead: {}",
                                task.chan_id, task.task_id, message
                            );
                            break 'outer;
                        };

                        if *ws_code == 200 {
                            warn!(
                                "{}/{}: Rebalance WAITSENDPAY_TIMEOUT failure after {}s: {}",
                                task.chan_id,
                                task.task_id,
                                now.elapsed().as_secs().to_string(),
                                message,
                            );
                            let temp_ban_route = &route[..route.len() - 1];
                            let mut source = temp_ban_route.first().unwrap().id;
                            for hop in temp_ban_route {
                                if hop.channel == temp_ban_route.first().unwrap().channel {
                                    source = hop.id;
                                } else {
                                    plugin
                                        .state()
                                        .graph
                                        .lock()
                                        .await
                                        .graph
                                        .get_mut(&source)
                                        .unwrap()
                                        .iter_mut()
                                        .find_map(|x| {
                                            if x.channel.short_channel_id == hop.channel
                                                && x.channel.destination != mypubkey
                                                && x.channel.source != mypubkey
                                            {
                                                x.liquidity = 0;
                                                x.timestamp = SystemTime::now()
                                                    .duration_since(UNIX_EPOCH)
                                                    .unwrap()
                                                    .as_secs();
                                                Some(x)
                                            } else {
                                                None
                                            }
                                        });
                                }
                            }
                            FailureReb {
                                amount_msat: job.amount_msat,
                                failure_reason: "WAITSENDPAY_TIMEOUT".to_string(),
                                failure_node: mypubkey,
                                channel_partner: match job.sat_direction {
                                    SatDirection::Pull => route.first().unwrap().channel,
                                    SatDirection::Push => route.last().unwrap().channel,
                                },
                                hops: (route.len() - 1) as u8,
                                created_at: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            }
                            .write_to_file(task.chan_id, &sling_dir)
                            .await?;
                            None
                        } else if let Some(d) = data {
                            let ws_error =
                                serde_json::from_value::<WaitsendpayErrorData>(d.clone())?;

                            info!(
                                "{}/{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                                task.chan_id,
                                task.task_id,
                                now.elapsed().as_secs().to_string(),
                                message,
                                ws_error.erring_node,
                                ws_error.erring_channel,
                            );

                            match &ws_error.failcodename {
                                err if err.eq("WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS")
                                    && ws_error.erring_node == mypubkey =>
                                {
                                    warn!(
                                        "{}/{}: PAYMENT DETAILS ERROR:{:?} {:?}",
                                        task.chan_id, task.task_id, err, route
                                    );
                                    special_stop = true;
                                }
                                _ => (),
                            }

                            FailureReb {
                                amount_msat: ws_error.amount_msat.unwrap().msat(),
                                failure_reason: ws_error.failcodename.clone(),
                                failure_node: ws_error.erring_node,
                                channel_partner: match job.sat_direction {
                                    SatDirection::Pull => route.first().unwrap().channel,
                                    SatDirection::Push => route.last().unwrap().channel,
                                },
                                hops: (route.len() - 1) as u8,
                                created_at: ws_error.created_at,
                            }
                            .write_to_file(task.chan_id, &sling_dir)
                            .await?;
                            if special_stop {
                                channel_jobstate_update(
                                    plugin.state().job_state.clone(),
                                    task,
                                    &JobMessage::Error,
                                    None,
                                    Some(false),
                                );
                                warn!(
                                    "{}/{}: UNEXPECTED waitsendpay failure after {}s: {}",
                                    task.chan_id,
                                    task.task_id,
                                    now.elapsed().as_secs().to_string(),
                                    message
                                );
                                break 'outer;
                            }

                            if ws_error.erring_channel == route.last().unwrap().channel {
                                warn!(
                                    "{}/{}: Last peer has a problem or just updated their fees? {}",
                                    task.chan_id, task.task_id, ws_error.failcodename
                                );
                                if message.contains("Too many HTLCs") {
                                    my_sleep(3, plugin.state().job_state.clone(), task).await;
                                } else {
                                    match job.sat_direction {
                                        SatDirection::Pull => {
                                            my_sleep(60, plugin.state().job_state.clone(), task)
                                                .await;
                                        }
                                        SatDirection::Push => {
                                            plugin.state().tempbans.lock().insert(
                                                route.last().unwrap().channel,
                                                SystemTime::now()
                                                    .duration_since(UNIX_EPOCH)
                                                    .unwrap()
                                                    .as_secs(),
                                            );
                                        }
                                    }
                                }
                            } else if ws_error.erring_channel == route.first().unwrap().channel {
                                warn!(
                                    "{}/{}: First peer has a problem {}",
                                    task.chan_id,
                                    task.task_id,
                                    message.clone()
                                );
                                if message.contains("Too many HTLCs") {
                                    my_sleep(3, plugin.state().job_state.clone(), task).await;
                                } else {
                                    match job.sat_direction {
                                        SatDirection::Pull => {
                                            plugin.state().tempbans.lock().insert(
                                                route.first().unwrap().channel,
                                                SystemTime::now()
                                                    .duration_since(UNIX_EPOCH)
                                                    .unwrap()
                                                    .as_secs(),
                                            );
                                        }
                                        SatDirection::Push => {
                                            my_sleep(60, plugin.state().job_state.clone(), task)
                                                .await;
                                        }
                                    }
                                }
                            } else {
                                debug!(
                                    "{}/{}: Adjusting liquidity for {}.",
                                    task.chan_id, task.task_id, ws_error.erring_channel
                                );
                                plugin
                                    .state()
                                    .graph
                                    .lock()
                                    .await
                                    .graph
                                    .get_mut(&ws_error.erring_node)
                                    .unwrap()
                                    .iter_mut()
                                    .find_map(|x| {
                                        if x.channel.short_channel_id == ws_error.erring_channel
                                            && x.channel.destination != mypubkey
                                            && x.channel.source != mypubkey
                                        {
                                            x.liquidity = ws_error.amount_msat.unwrap().msat() - 1;
                                            x.timestamp = SystemTime::now()
                                                .duration_since(UNIX_EPOCH)
                                                .unwrap()
                                                .as_secs();
                                            Some(x)
                                        } else {
                                            None
                                        }
                                    });
                            }
                            Some(ws_error.erring_channel)
                        } else {
                            channel_jobstate_update(
                                plugin.state().job_state.clone(),
                                task,
                                &JobMessage::Error,
                                None,
                                Some(false),
                            );
                            warn!(
                                "{}/{}: UNEXPECTED waitsendpay failure: {} after: {}",
                                task.chan_id,
                                task.task_id,
                                message,
                                now.elapsed().as_millis().to_string()
                            );
                            break 'outer;
                        }
                    }
                    Err(e) => {
                        channel_jobstate_update(
                            plugin.state().job_state.clone(),
                            task,
                            &JobMessage::Error,
                            None,
                            Some(false),
                        );
                        warn!(
                            "{}/{}: UNEXPECTED waitsendpay error type: {} after: {}",
                            task.chan_id,
                            task.task_id,
                            e,
                            now.elapsed().as_millis().to_string()
                        );
                        break 'outer;
                    }
                }
            }
        };

        if match err_chan {
            Some(ec) => ec != route_claim_chan,
            None => true,
        } {
            plugin
                .state()
                .graph
                .lock()
                .await
                .graph
                .get_mut(&route_claim_peer)
                .unwrap()
                .iter_mut()
                .find_map(|x| {
                    if x.channel.short_channel_id == route_claim_chan
                        && x.channel.destination != mypubkey
                        && x.channel.source != mypubkey
                    {
                        x.liquidity = route_claim_liq;
                        Some(x)
                    } else {
                        None
                    }
                });
        }
    }

    Ok(())
}

async fn next_route(
    plugin: &Plugin<PluginState>,
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
    job: &Job,
    tempbans: &HashMap<ShortChannelId, u64>,
    task: &Task,
    keypair: &PublicKeyPair,
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<Vec<SendpayRoute>, Error> {
    let graph = plugin.state().graph.lock().await;
    let config = plugin.state().config.lock().clone();
    #[allow(clippy::clone_on_copy)]
    let blockheight = plugin.state().blockheight.lock().clone();
    let candidatelist;
    match job.candidatelist {
        Some(ref c) => {
            if !c.is_empty() {
                candidatelist =
                    build_candidatelist(peer_channels, job, tempbans, &config, Some(c), blockheight)
            } else {
                candidatelist =
                    build_candidatelist(peer_channels, job, tempbans, &config, None, blockheight)
            }
        }
        None => {
            candidatelist =
                build_candidatelist(peer_channels, job, tempbans, &config, None, blockheight)
        }
    }
    debug!(
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
        debug!(
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
        info!(
            "{}/{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
            task.chan_id, task.task_id
        );
        channel_jobstate_update(
            plugin.state().job_state.clone(),
            task,
            &JobMessage::NoCandidates,
            None,
            None,
        );
        return Err(anyhow!("No candidates found"));
    }

    let mut route = Vec::new();
    match success_route {
        Some(prev_route) => {
            if match job.sat_direction {
                SatDirection::Pull => candidatelist
                    .iter()
                    .any(|c| c == &prev_route.first().unwrap().channel),
                SatDirection::Push => candidatelist
                    .iter()
                    .any(|c| c == &prev_route.last().unwrap().channel),
            } {
                route = prev_route.clone();
            } else {
                *success_route = None;
            }
        }
        None => (),
    }
    match success_route {
        Some(_) => (),
        None => {
            let slingchan_inc = match graph.get_channel(&keypair.other_pubkey, &task.chan_id) {
                Ok(in_chan) => in_chan,
                Err(_) => {
                    warn!(
                        "{}/{}: channel not found in graph!",
                        task.chan_id, task.task_id
                    );
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        task,
                        &JobMessage::ChanNotInGraph,
                        None,
                        None,
                    );
                    return Err(anyhow!("channel not found in graph"));
                }
            };
            let slingchan_out = match graph.get_channel(&keypair.my_pubkey, &task.chan_id) {
                Ok(out_chan) => out_chan,
                Err(_) => {
                    warn!(
                        "{}/{}: channel not found in graph!",
                        task.chan_id, task.task_id
                    );
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        task,
                        &JobMessage::ChanNotInGraph,
                        None,
                        None,
                    );
                    return Err(anyhow!("channel not found in graph!"));
                }
            };

            let mut pull_jobs = plugin.state().pull_jobs.lock().clone();
            let mut push_jobs = plugin.state().push_jobs.lock().clone();
            let excepts = plugin.state().excepts_chans.lock().clone();
            let excepts_peers = plugin.state().excepts_peers.lock().clone();
            for except in &excepts {
                pull_jobs.insert(*except);
                push_jobs.insert(*except);
            }
            let last_delay = match config.cltv_delta.value {
                Some(c) => max(144, c),
                None => 144,
            };
            let max_hops = match job.maxhops {
                Some(h) => h + 1,
                None => config.maxhops.value + 1,
            };
            match job.sat_direction {
                SatDirection::Pull => {
                    route = dijkstra(
                        &keypair.my_pubkey,
                        &graph,
                        &keypair.my_pubkey,
                        &keypair.other_pubkey,
                        &DijkstraNode {
                            score: 0,
                            destination: keypair.my_pubkey,
                            channel: slingchan_inc.channel,
                            hops: 0,
                        },
                        job,
                        &candidatelist,
                        max_hops,
                        &ExcludeGraph {
                            exclude_chans: pull_jobs,
                            exclude_peers: excepts_peers,
                        },
                        last_delay,
                        tempbans,
                    )?;
                }
                SatDirection::Push => {
                    route = dijkstra(
                        &keypair.my_pubkey,
                        &graph,
                        &keypair.other_pubkey,
                        &keypair.my_pubkey,
                        &DijkstraNode {
                            score: 0,
                            destination: keypair.other_pubkey,
                            channel: slingchan_out.channel,
                            hops: 0,
                        },
                        job,
                        &candidatelist,
                        max_hops,
                        &ExcludeGraph {
                            exclude_chans: push_jobs,
                            exclude_peers: excepts_peers,
                        },
                        last_delay,
                        tempbans,
                    )?;
                }
            }
        }
    }
    Ok(route)
}

async fn health_check(
    plugin: Plugin<PluginState>,
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
    task: &Task,
    job: &Job,
    other_peer: PublicKey,
    tempbans: &HashMap<ShortChannelId, u64>,
) -> Option<bool> {
    let config = plugin.state().config.lock().clone();
    let job_states = plugin.state().job_state.clone();
    let our_listpeers_channel =
        get_normal_channel_from_listpeerchannels(peer_channels, &task.chan_id);
    if let Some(channel) = our_listpeers_channel {
        if is_channel_normal(&channel) {
            if job.is_balanced(&channel, &task.chan_id)
                || match job.sat_direction {
                    SatDirection::Pull => {
                        Amount::msat(&channel.receivable_msat.unwrap()) < job.amount_msat
                    }
                    SatDirection::Push => {
                        Amount::msat(&channel.spendable_msat.unwrap()) < job.amount_msat
                    }
                }
            {
                info!(
                    "{}/{}: already balanced. Taking a break...",
                    task.chan_id, task.task_id
                );
                channel_jobstate_update(
                    job_states.clone(),
                    task,
                    &JobMessage::Balanced,
                    None,
                    None,
                );
                my_sleep(600, job_states.clone(), task).await;
                Some(true)
            } else if get_total_htlc_count(&channel) > config.max_htlc_count.value {
                info!(
                    "{}/{}: already more than {} pending htlcs. Taking a break...",
                    task.chan_id, task.task_id, config.max_htlc_count.value
                );
                channel_jobstate_update(
                    job_states.clone(),
                    task,
                    &JobMessage::HTLCcapped,
                    None,
                    None,
                );
                my_sleep(10, job_states.clone(), task).await;
                Some(true)
            } else {
                match peer_channels
                    .values()
                    .find(|x| x.peer_id.unwrap() == other_peer)
                {
                    Some(p) => {
                        if !p.peer_connected.unwrap() {
                            info!(
                                "{}/{}: not connected. Taking a break...",
                                task.chan_id, task.task_id
                            );
                            channel_jobstate_update(
                                job_states.clone(),
                                task,
                                &JobMessage::Disconnected,
                                None,
                                None,
                            );
                            my_sleep(60, job_states.clone(), task).await;
                            Some(true)
                        } else if match job.sat_direction {
                            SatDirection::Pull => false,
                            SatDirection::Push => true,
                        } && tempbans.contains_key(&task.chan_id)
                        {
                            info!(
                                "{}/{}: First peer not ready. Taking a break...",
                                task.chan_id, task.task_id
                            );
                            channel_jobstate_update(
                                job_states.clone(),
                                task,
                                &JobMessage::PeerNotReady,
                                None,
                                None,
                            );
                            my_sleep(20, job_states.clone(), task).await;
                            Some(true)
                        } else {
                            None
                        }
                    }
                    None => {
                        channel_jobstate_update(
                            job_states.clone(),
                            task,
                            &JobMessage::PeerNotFound,
                            Some(false),
                            None,
                        );
                        warn!(
                            "{}/{}: peer not found. Stopping job.",
                            task.chan_id, task.task_id
                        );
                        Some(false)
                    }
                }
            }
        } else {
            warn!(
                "{}/{}: not in CHANNELD_NORMAL state. Stopping Job.",
                task.chan_id, task.task_id
            );
            channel_jobstate_update(
                job_states.clone(),
                task,
                &JobMessage::ChanNotNormal,
                Some(false),
                None,
            );
            Some(false)
        }
    } else {
        warn!(
            "{}/{}: not found. Stopping Job.",
            task.chan_id, task.task_id
        );
        channel_jobstate_update(
            job_states.clone(),
            task,
            &JobMessage::ChanNotNormal,
            Some(false),
            None,
        );
        Some(false)
    }
}

fn build_candidatelist(
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
    job: &Job,
    tempbans: &HashMap<ShortChannelId, u64>,
    config: &Config,
    custom_candidates: Option<&Vec<ShortChannelId>>,
    blockheight: u32,
) -> Vec<ShortChannelId> {
    let mut candidatelist = Vec::<ShortChannelId>::new();

    let depleteuptopercent = match job.depleteuptopercent {
        Some(dp) => dp,
        None => config.depleteuptopercent.value,
    };
    let depleteuptoamount = match job.depleteuptoamount {
        Some(dp) => dp,
        None => config.depleteuptoamount.value,
    };

    for channel in peer_channels.values() {
        if let Some(scid) = channel.short_channel_id {
            if matches!(
                channel.state.unwrap(),
                ListpeerchannelsChannelsState::CHANNELD_NORMAL
            ) && channel.peer_connected.unwrap()
                && match custom_candidates {
                    Some(c) => c.iter().any(|c| *c == scid),
                    None => true,
                }
                && scid.block() <= blockheight - config.candidates_min_age.value
            {
                let chan_updates = if let Some(updates) = &channel.updates {
                    if let Some(remote) = &updates.remote {
                        remote
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };
                let chan_in_ppm = feeppm_effective(
                    chan_updates.fee_proportional_millionths.unwrap(),
                    Amount::msat(&chan_updates.fee_base_msat.unwrap()) as u32,
                    job.amount_msat,
                );

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
                    && get_total_htlc_count(channel) <= config.max_htlc_count.value
                {
                    candidatelist.push(scid);
                }
            }
        }
    }

    candidatelist
}
