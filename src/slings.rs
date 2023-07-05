use anyhow::{anyhow, Error};

use cln_plugin::Plugin;

use cln_rpc::{model::*, primitives::*};

use log::{debug, info, warn};

use parking_lot::Mutex;
use sling::{Job, SatDirection};
use std::cmp::{max, min};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use std::time::SystemTime;
use std::{collections::HashMap, time::UNIX_EPOCH};
use std::{path::PathBuf, str::FromStr};

use tokio::time::Instant;

use crate::dijkstra::dijkstra;
use crate::model::{
    Config, DijkstraNode, FailureReb, JobMessage, JobState, LnGraph, PluginState, SuccessReb,
    PLUGIN_NAME,
};
use crate::rpc::{slingsend, waitsendpay};
use crate::util::{
    channel_jobstate_update, feeppm_effective, feeppm_effective_from_amts,
    get_normal_channel_from_listpeerchannels, get_preimage_paymend_hash_pair, get_total_htlc_count,
    is_channel_normal, my_sleep,
};

pub async fn sling(
    rpc_path: &PathBuf,
    chan_id: ShortChannelId,
    job: Job,
    task_id: u8,
    plugin: &Plugin<PluginState>,
) -> Result<(), Error> {
    let config = plugin.state().config.lock().clone();
    let mypubkey = config.pubkey.unwrap().clone();

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let mut networkdir = PathBuf::from_str(&plugin.configuration().lightning_dir.clone()).unwrap();
    networkdir.pop();

    let last_delay = match config.cltv_delta.1 {
        Some(c) => max(144, c),
        None => 144,
    };

    loop {
        {
            let graph = plugin.state().graph.lock().await;

            if graph.graph.len() == 0 {
                info!(
                    "{}/{}: graph is still empty. Sleeping...",
                    chan_id.to_string(),
                    task_id
                );
                channel_jobstate_update(
                    plugin.state().job_state.clone(),
                    chan_id,
                    task_id,
                    JobMessage::GraphEmpty,
                    None,
                    None,
                );
            } else {
                break;
            }
        }
        my_sleep(600, plugin.state().job_state.clone(), &chan_id, task_id).await;
    }
    let other_peer;
    {
        let peer_channels = plugin.state().peer_channels.lock().await;

        other_peer = peer_channels
            .get(&chan_id.to_string())
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
            .get(&chan_id.to_string())
            .unwrap()
            .iter()
            .find(|jt| jt.id() == task_id)
            .unwrap()
            .should_stop();
        if should_stop {
            info!("{}/{}: Stopped job!", chan_id.to_string(), task_id);
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                task_id,
                JobMessage::Stopped,
                Some(false),
                None,
            );
            break;
        }

        let tempbans = plugin.state().tempbans.lock().clone();
        let peer_channels = plugin.state().peer_channels.lock().await.clone();

        match health_check(
            &peer_channels,
            chan_id,
            task_id,
            &job,
            other_peer,
            plugin.state().job_state.clone(),
            &config,
            &tempbans,
        )
        .await
        {
            Some(r) => {
                if r {
                    continue 'outer;
                } else {
                    break 'outer;
                }
            }
            None => (),
        }

        channel_jobstate_update(
            plugin.state().job_state.clone(),
            chan_id,
            task_id,
            JobMessage::Rebalancing,
            None,
            None,
        );

        let route = match next_route(
            plugin,
            &peer_channels,
            &job,
            &tempbans,
            &config,
            &chan_id,
            task_id,
            &other_peer,
            &mut success_route,
            &mypubkey,
            last_delay,
        )
        .await
        {
            Ok(r) => r,
            Err(_e) => {
                success_route = None;
                my_sleep(600, plugin.state().job_state.clone(), &chan_id, task_id).await;
                continue 'outer;
            }
        };

        if route.len() == 0 {
            info!(
                "{}/{}: could not find a route. Sleeping...",
                chan_id.to_string(),
                task_id
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                task_id,
                JobMessage::NoRoute,
                None,
                None,
            );
            success_route = None;
            my_sleep(600, plugin.state().job_state.clone(), &chan_id, task_id).await;
            continue 'outer;
        }

        let fee_ppm_effective = feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        );
        info!(
            "{}/{}: Found {}ppm route with {} hops. Total: {}ms",
            chan_id.to_string(),
            task_id,
            fee_ppm_effective,
            route.len() - 1,
            now.elapsed().as_millis().to_string()
        );

        if fee_ppm_effective > job.maxppm {
            info!(
                "{}/{}: route not cheap enough! Sleeping...",
                chan_id.to_string(),
                task_id
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                task_id,
                JobMessage::TooExp,
                None,
                None,
            );
            my_sleep(600, plugin.state().job_state.clone(), &chan_id, task_id).await;
            success_route = None;
            continue 'outer;
        }

        {
            let alias_map = plugin.state().alias_peer_map.lock();
            for r in &route {
                debug!(
                    "{}/{}: route: {} {:3} {:17} {}",
                    chan_id.to_string(),
                    task_id,
                    Amount::msat(&r.amount_msat),
                    r.delay,
                    r.channel.to_string(),
                    alias_map.get(&r.id).unwrap_or(&r.id.to_string()),
                );
            }
        }

        let route_claim_chan = route[route.len() / 2].channel;
        let route_claim_peer = route[(route.len() / 2) - 1].id;
        debug!(
            "{}/{}: setting liquidity on {} to 0 to not get same route for parallel tasks.",
            chan_id.to_string(),
            task_id,
            route_claim_chan.to_string()
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
        match slingsend(&rpc_path, route.clone(), payment_hash, None, None).await {
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
                        chan_id.to_string(),
                        task_id
                    );
                    plugin.state().tempbans.lock().insert(
                        route.first().unwrap().channel.to_string(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    );
                    success_route = None;
                    FailureReb {
                        amount_msat: job.amount,
                        failure_reason: "FIRST_PEER_NOT_READY".to_string(),
                        failure_node: route.first().unwrap().id.clone(),
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
                    .write_to_file(chan_id, &sling_dir)
                    .await?;
                    continue;
                } else {
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        chan_id,
                        task_id,
                        JobMessage::Error,
                        None,
                        Some(false),
                    );
                    warn!(
                        "{}/{}: Unexpected sendpay error: {}",
                        chan_id.to_string(),
                        task_id,
                        e.to_string()
                    );
                    break 'outer;
                }
            }
        };
        info!(
            "{}/{}: Sent on route. Total: {}ms",
            chan_id.to_string(),
            task_id,
            now.elapsed().as_millis().to_string()
        );

        let mut err_chan = None;
        match waitsendpay(
            &networkdir,
            &config.lightning_cli.1,
            send_response.payment_hash,
            config.timeoutpay.1,
        )
        .await
        {
            Ok(o) => {
                info!(
                    "{}/{}: Rebalance SUCCESSFULL after {}s. Sent {}sats plus {}msats fee",
                    chan_id.to_string(),
                    task_id,
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
                .write_to_file(chan_id, &sling_dir)
                .await?;
                success_route = Some(route);
            }
            Err(e) => {
                success_route = None;
                plugin
                    .state()
                    .pays
                    .write()
                    .remove(&send_response.payment_hash.to_string());
                let mut special_stop = false;
                match e.code {
                    Some(c) => {
                        if c == 200 {
                            warn!(
                                "{}/{}: Rebalance WAITSENDPAY_TIMEOUT failure after {}s: {}",
                                chan_id.to_string(),
                                task_id,
                                now.elapsed().as_secs().to_string(),
                                e.message,
                            );
                            let temp_ban_route = &route[..route.len() - 1];
                            let mut source = temp_ban_route.first().unwrap().id;
                            for hop in temp_ban_route {
                                if hop.channel.to_string()
                                    == temp_ban_route.first().unwrap().channel.to_string()
                                {
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
                                            if x.channel.short_channel_id.to_string()
                                                == hop.channel.to_string()
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
                                amount_msat: job.amount,
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
                            .write_to_file(chan_id, &sling_dir)
                            .await?;
                        } else {
                            match e.data {
                                Some(ref data) => {
                                    err_chan = Some(data.erring_channel);
                                    info!(
                                        "{}/{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                                        chan_id.to_string(),
                                        task_id,
                                        now.elapsed().as_secs().to_string(),
                                        e.message,
                                        data.erring_node,
                                        data.erring_channel.to_string()
                                    );
                                    match &data.failcodename {
                                        err if err
                                            .eq("WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS")
                                            && data.erring_node == mypubkey =>
                                        {
                                            warn!(
                                                "{}/{}: PAYMENT DETAILS ERROR:{:?} {:?}",
                                                chan_id.to_string(),
                                                task_id,
                                                e,
                                                route
                                            );
                                            special_stop = true;
                                        }
                                        _ => (),
                                    }

                                    FailureReb {
                                        amount_msat: Amount::msat(&data.amount_msat.unwrap()),
                                        failure_reason: data.failcodename.clone(),
                                        failure_node: data.erring_node,
                                        channel_partner: match job.sat_direction {
                                            SatDirection::Pull => route.first().unwrap().channel,
                                            SatDirection::Push => route.last().unwrap().channel,
                                        },
                                        hops: (route.len() - 1) as u8,
                                        created_at: data.created_at,
                                    }
                                    .write_to_file(chan_id, &sling_dir)
                                    .await?;
                                    if special_stop {
                                        channel_jobstate_update(
                                            plugin.state().job_state.clone(),
                                            chan_id,
                                            task_id,
                                            JobMessage::Error,
                                            None,
                                            Some(false),
                                        );
                                        warn!(
                                            "{}/{}: UNEXPECTED waitsendpay failure after {}s: {:?}",
                                            chan_id.to_string(),
                                            task_id,
                                            now.elapsed().as_secs().to_string(),
                                            e
                                        );
                                        break 'outer;
                                    }

                                    if data.erring_channel.to_string()
                                        == route.last().unwrap().channel.to_string()
                                    {
                                        warn!(
                                            "{}/{}: Last peer has a problem or just updated their fees? {}",
                                            chan_id.to_string(),
                                            task_id,
                                            data.failcodename
                                        );
                                        if e.message.contains("Too many HTLCs") {
                                            my_sleep(
                                                3,
                                                plugin.state().job_state.clone(),
                                                &chan_id,
                                                task_id,
                                            )
                                            .await;
                                        } else {
                                            match job.sat_direction {
                                                SatDirection::Pull => {
                                                    plugin.state().tempbans.lock().insert(
                                                        route
                                                            .get(route.len() - 3)
                                                            .unwrap()
                                                            .channel
                                                            .to_string(),
                                                        SystemTime::now()
                                                            .duration_since(UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_secs(),
                                                    );
                                                }
                                                SatDirection::Push => {
                                                    plugin.state().tempbans.lock().insert(
                                                        route.last().unwrap().channel.to_string(),
                                                        SystemTime::now()
                                                            .duration_since(UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_secs(),
                                                    );
                                                }
                                            }
                                        }
                                    } else if data.erring_channel.to_string()
                                        == route.first().unwrap().channel.to_string()
                                    {
                                        warn!(
                                            "{}/{}: First peer has a problem {}",
                                            chan_id.to_string(),
                                            task_id,
                                            e.message.clone()
                                        );
                                        if e.message.contains("Too many HTLCs") {
                                            my_sleep(
                                                3,
                                                plugin.state().job_state.clone(),
                                                &chan_id,
                                                task_id,
                                            )
                                            .await;
                                        } else {
                                            match job.sat_direction {
                                                SatDirection::Pull => {
                                                    plugin.state().tempbans.lock().insert(
                                                        route.first().unwrap().channel.to_string(),
                                                        SystemTime::now()
                                                            .duration_since(UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_secs(),
                                                    );
                                                }
                                                SatDirection::Push => {
                                                    my_sleep(
                                                        60,
                                                        plugin.state().job_state.clone(),
                                                        &chan_id,
                                                        task_id,
                                                    )
                                                    .await;
                                                }
                                            }
                                        }
                                    } else {
                                        debug!(
                                            "{}/{}: Adjusting liquidity for {}.",
                                            chan_id.to_string(),
                                            task_id,
                                            data.erring_channel.to_string()
                                        );
                                        plugin
                                            .state()
                                            .graph
                                            .lock()
                                            .await
                                            .graph
                                            .get_mut(&data.erring_node)
                                            .unwrap()
                                            .iter_mut()
                                            .find_map(|x| {
                                                if x.channel.short_channel_id == data.erring_channel
                                                    && x.channel.destination != mypubkey
                                                    && x.channel.source != mypubkey
                                                {
                                                    x.liquidity =
                                                        Amount::msat(&data.amount_msat.unwrap())
                                                            - 1;
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
                                None => {
                                    channel_jobstate_update(
                                        plugin.state().job_state.clone(),
                                        chan_id,
                                        task_id,
                                        JobMessage::Error,
                                        None,
                                        Some(false),
                                    );
                                    warn!(
                                        "{}/{}: UNEXPECTED waitsendpay failure after {}s: {:?}",
                                        chan_id.to_string(),
                                        task_id,
                                        now.elapsed().as_secs().to_string(),
                                        e
                                    );
                                    break 'outer;
                                }
                            };
                        }
                    }
                    None => (),
                }
            }
        };
        if match err_chan {
            Some(ec) => {
                if ec != route_claim_chan {
                    true
                } else {
                    false
                }
            }
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
    peer_channels: &BTreeMap<String, ListpeerchannelsChannels>,
    job: &Job,
    tempbans: &HashMap<String, u64>,
    config: &Config,
    chan_id: &ShortChannelId,
    task_id: u8,
    other_peer: &PublicKey,
    success_route: &mut Option<Vec<SendpayRoute>>,
    mypubkey: &PublicKey,
    last_delay: u16,
) -> Result<Vec<SendpayRoute>, Error> {
    let graph = plugin.state().graph.lock().await;
    let candidatelist;
    match job.candidatelist {
        Some(ref c) => {
            if c.len() > 0 {
                candidatelist =
                    build_candidatelist(&peer_channels, &job, &graph, &tempbans, &config, Some(c))
            } else {
                candidatelist =
                    build_candidatelist(&peer_channels, &job, &graph, &tempbans, &config, None)
            }
        }
        None => {
            candidatelist =
                build_candidatelist(&peer_channels, &job, &graph, &tempbans, &config, None)
        }
    }
    debug!(
        "{}/{}: Candidates: {}",
        chan_id.to_string(),
        task_id,
        candidatelist
            .iter()
            .map(|y| y.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    );
    if tempbans.len() > 0 {
        debug!(
            "{}/{}: Tempbans: {}",
            chan_id.to_string(),
            task_id,
            plugin
                .state()
                .tempbans
                .lock()
                .clone()
                .keys()
                .map(|y| y.to_owned())
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    if candidatelist.len() == 0 {
        info!(
            "{}/{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
            chan_id.to_string(),
            task_id
        );
        channel_jobstate_update(
            plugin.state().job_state.clone(),
            *chan_id,
            task_id,
            JobMessage::NoCandidates,
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
                    .any(|c| c.to_string() == prev_route.first().unwrap().channel.to_string()),
                SatDirection::Push => candidatelist
                    .iter()
                    .any(|c| c.to_string() == prev_route.last().unwrap().channel.to_string()),
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
            let slingchan_inc;
            match graph.get_channel(other_peer, chan_id) {
                Ok(in_chan) => slingchan_inc = in_chan,
                Err(_) => {
                    warn!(
                        "{}/{}: channel not found in graph!",
                        chan_id.to_string(),
                        task_id
                    );
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        *chan_id,
                        task_id,
                        JobMessage::ChanNotInGraph,
                        None,
                        None,
                    );
                    return Err(anyhow!("channel not found in graph"));
                }
            };
            let slingchan_out;
            match graph.get_channel(mypubkey, chan_id) {
                Ok(out_chan) => slingchan_out = out_chan,
                Err(_) => {
                    warn!(
                        "{}/{}: channel not found in graph!",
                        chan_id.to_string(),
                        task_id
                    );
                    channel_jobstate_update(
                        plugin.state().job_state.clone(),
                        *chan_id,
                        task_id,
                        JobMessage::ChanNotInGraph,
                        None,
                        None,
                    );
                    return Err(anyhow!("channel not found in graph!"));
                }
            };

            let maxhops = match job.maxhops {
                Some(h) => h + 1,
                None => 9,
            };

            let mut pull_jobs = plugin.state().pull_jobs.lock().clone();
            let mut push_jobs = plugin.state().push_jobs.lock().clone();
            let excepts = plugin.state().excepts_chans.lock().clone();
            let excepts_peers = plugin.state().excepts_peers.lock().clone();
            for except in &excepts {
                pull_jobs.insert(except.to_string());
                push_jobs.insert(except.to_string());
            }

            match job.sat_direction {
                SatDirection::Pull => {
                    route = dijkstra(
                        &mypubkey,
                        &graph,
                        mypubkey,
                        other_peer,
                        &DijkstraNode {
                            score: 0,
                            destination: *mypubkey,
                            channel: slingchan_inc.channel.clone(),
                            hops: 0,
                        },
                        &job,
                        &candidatelist,
                        &pull_jobs,
                        &excepts_peers,
                        maxhops,
                        last_delay,
                        &tempbans,
                    )?;
                }
                SatDirection::Push => {
                    route = dijkstra(
                        &mypubkey,
                        &graph,
                        other_peer,
                        mypubkey,
                        &DijkstraNode {
                            score: 0,
                            destination: *other_peer,
                            channel: slingchan_out.channel.clone(),
                            hops: 0,
                        },
                        &job,
                        &candidatelist,
                        &push_jobs,
                        &excepts_peers,
                        maxhops,
                        last_delay,
                        &tempbans,
                    )?;
                }
            }
        }
    }
    Ok(route)
}

async fn health_check(
    peer_channels: &BTreeMap<String, ListpeerchannelsChannels>,
    chan_id: ShortChannelId,
    task_id: u8,
    job: &Job,
    other_peer: PublicKey,
    job_states: Arc<Mutex<HashMap<String, Vec<JobState>>>>,
    config: &Config,
    tempbans: &HashMap<String, u64>,
) -> Option<bool> {
    let our_listpeers_channel =
        get_normal_channel_from_listpeerchannels(&peer_channels, &chan_id.to_string());
    if let Some(channel) = our_listpeers_channel {
        if is_channel_normal(&channel) {
            if job.is_balanced(&channel, &chan_id)
                || match job.sat_direction {
                    SatDirection::Pull => {
                        Amount::msat(&channel.receivable_msat.unwrap()) < job.amount
                    }
                    SatDirection::Push => {
                        Amount::msat(&channel.spendable_msat.unwrap()) < job.amount
                    }
                }
            {
                info!(
                    "{}/{}: already balanced. Taking a break...",
                    chan_id.to_string(),
                    task_id
                );
                channel_jobstate_update(
                    job_states.clone(),
                    chan_id,
                    task_id,
                    JobMessage::Balanced,
                    None,
                    None,
                );
                my_sleep(600, job_states.clone(), &chan_id, task_id).await;
                Some(true)
            } else if get_total_htlc_count(&channel) > config.max_htlc_count.1 {
                info!(
                    "{}/{}: already more than {} pending htlcs. Taking a break...",
                    chan_id.to_string(),
                    task_id,
                    config.max_htlc_count.1
                );
                channel_jobstate_update(
                    job_states.clone(),
                    chan_id,
                    task_id,
                    JobMessage::HTLCcapped,
                    None,
                    None,
                );
                my_sleep(10, job_states.clone(), &chan_id, task_id).await;
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
                                chan_id.to_string(),
                                task_id
                            );
                            channel_jobstate_update(
                                job_states.clone(),
                                chan_id,
                                task_id,
                                JobMessage::Disconnected,
                                None,
                                None,
                            );
                            my_sleep(60, job_states.clone(), &chan_id, task_id).await;
                            Some(true)
                        } else if match job.sat_direction {
                            SatDirection::Pull => false,
                            SatDirection::Push => true,
                        } && tempbans.contains_key(&chan_id.to_string())
                        {
                            info!(
                                "{}/{}: First peer not ready. Taking a break...",
                                chan_id.to_string(),
                                task_id
                            );
                            channel_jobstate_update(
                                job_states.clone(),
                                chan_id,
                                task_id,
                                JobMessage::PeerNotReady,
                                None,
                                None,
                            );
                            my_sleep(20, job_states.clone(), &chan_id, task_id).await;
                            Some(true)
                        } else {
                            None
                        }
                    }
                    None => {
                        channel_jobstate_update(
                            job_states.clone(),
                            chan_id,
                            task_id,
                            JobMessage::PeerNotFound,
                            Some(false),
                            None,
                        );
                        warn!(
                            "{}/{}: peer not found. Stopping job.",
                            chan_id.to_string(),
                            task_id
                        );
                        Some(false)
                    }
                }
            }
        } else {
            warn!(
                "{}/{}: not in CHANNELD_NORMAL state. Stopping Job.",
                chan_id.to_string(),
                task_id
            );
            channel_jobstate_update(
                job_states.clone(),
                chan_id,
                task_id,
                JobMessage::ChanNotNormal,
                Some(false),
                None,
            );
            Some(false)
        }
    } else {
        warn!(
            "{}/{}: not found. Stopping Job.",
            chan_id.to_string(),
            task_id
        );
        channel_jobstate_update(
            job_states.clone(),
            chan_id,
            task_id,
            JobMessage::ChanNotNormal,
            Some(false),
            None,
        );
        Some(false)
    }
}

fn build_candidatelist(
    peer_channels: &BTreeMap<String, ListpeerchannelsChannels>,
    job: &Job,
    graph: &LnGraph,
    tempbans: &HashMap<String, u64>,
    config: &Config,
    custom_candidates: Option<&Vec<ShortChannelId>>,
) -> Vec<ShortChannelId> {
    let mut candidatelist = Vec::<ShortChannelId>::new();

    let depleteuptopercent = match job.depleteuptopercent {
        Some(dp) => dp,
        None => config.depleteuptopercent.1,
    };
    let depleteuptoamount = match job.depleteuptoamount {
        Some(dp) => dp,
        None => config.depleteuptoamount.1,
    };

    for channel in peer_channels.values() {
        if let Some(scid) = channel.short_channel_id {
            if match channel.state.unwrap() {
                ListpeerchannelsChannelsState::CHANNELD_NORMAL => true,
                _ => false,
            } && channel.peer_connected.unwrap()
                && match custom_candidates {
                    Some(c) => c.iter().any(|c| *c == scid),
                    None => true,
                }
            {
                let chan_from_peer = match graph.get_channel(&channel.peer_id.unwrap(), &scid) {
                    Ok(chan) => chan.channel,
                    Err(_) => continue,
                };

                let to_us_msat = Amount::msat(&channel.to_us_msat.unwrap());
                let total_msat = Amount::msat(&channel.total_msat.unwrap());
                let chan_out_ppm = feeppm_effective(
                    channel.fee_proportional_millionths.unwrap(),
                    Amount::msat(&channel.fee_base_msat.unwrap()) as u32,
                    job.amount,
                );
                let chan_in_ppm = feeppm_effective(
                    chan_from_peer.fee_per_millionth,
                    chan_from_peer.base_fee_millisatoshi,
                    job.amount,
                );
                if match job.sat_direction {
                    SatDirection::Pull => {
                        to_us_msat
                            > max(
                                job.amount + 10_000_000,
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
                                job.amount + 10_000_000,
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
                } && !tempbans.contains_key(&scid.to_string())
                    && get_total_htlc_count(channel) <= config.max_htlc_count.1
                {
                    candidatelist.push(scid.clone());
                }
            }
        }
    }

    candidatelist
}
