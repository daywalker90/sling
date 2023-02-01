use anyhow::Error;

use cln_plugin::Plugin;

use cln_rpc::{model::*, primitives::*};

use log::{debug, info, warn};

use parking_lot::Mutex;

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use std::{
    collections::HashMap,
    time::{Duration, UNIX_EPOCH},
};
use std::{path::PathBuf, str::FromStr};

use tokio::time::{self, Instant};

use crate::dijkstra::dijkstra;
use crate::model::{
    DijkstraNode, FailureReb, Job, JobMessage, JobState, LnGraph, SatDirection, SuccessReb,
};
use crate::util::{
    channel_jobstate_update, feeppm_effective, feeppm_effective_from_amts, get_in_htlc_count,
    get_out_htlc_count, get_peer_id_from_chan_id, get_preimage_paymend_hash_pair,
};
use crate::{
    delpay, get_normal_channel_from_listpeers, slingsend, waitsendpay, PluginState, PLUGIN_NAME,
};

pub async fn sling(
    rpc_path: &PathBuf,
    chan_id: ShortChannelId,
    job: Job,
    plugin: &Plugin<PluginState>,
) -> Result<(), Error> {
    let config = plugin.state().config.lock().clone();
    let mypubkey = config.pubkey.unwrap().clone();

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    let mut networkdir = PathBuf::from_str(&plugin.configuration().lightning_dir.clone()).unwrap();
    networkdir.pop();

    let mut success_route: Option<Vec<SendpayRoute>> = None;
    'outer: loop {
        let now = Instant::now();
        let should_stop = plugin
            .state()
            .job_state
            .lock()
            .get(&chan_id.to_string())
            .unwrap()
            .should_stop();
        if should_stop {
            info!("{}: Stopped job!", chan_id.to_string());
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                JobMessage::Stopped,
                Some(false),
                None,
            );
            break;
        }

        let peers = plugin.state().peers.lock().clone();

        let other_peer = get_peer_id_from_chan_id(&peers, chan_id)?;

        match health_check(
            chan_id,
            &job,
            &peers,
            other_peer,
            plugin.state().job_state.clone(),
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

        let graph = plugin.state().graph.lock().clone();

        if graph.graph.len() == 0 {
            info!("{}: graph is still empty. Sleeping...", chan_id.to_string());
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                JobMessage::GraphEmpty,
                None,
                None,
            );
            time::sleep(Duration::from_secs(10)).await;
            continue 'outer;
        }

        channel_jobstate_update(
            plugin.state().job_state.clone(),
            chan_id,
            JobMessage::Rebalancing,
            None,
            None,
        );

        let tempbans = plugin.state().tempbans.lock().clone();
        let candidatelist;
        match job.candidatelist {
            Some(ref c) => {
                if c.len() > 0 {
                    candidatelist = c.clone()
                } else {
                    candidatelist = build_candidatelist(&peers, &job, &graph, tempbans)
                }
            }
            None => candidatelist = build_candidatelist(&peers, &job, &graph, tempbans),
        }
        debug!(
            "{}: Candidates: {}",
            chan_id.to_string(),
            candidatelist
                .iter()
                .map(|y| y.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        );
        debug!(
            "{}: Tempbans: {}",
            chan_id.to_string(),
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

        if candidatelist.len() == 0 {
            info!(
                "{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
                chan_id.to_string()
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                JobMessage::NoCandidates,
                None,
                None,
            );
            time::sleep(Duration::from_secs(20)).await;
            continue 'outer;
        }

        let mut route = Vec::new();
        match success_route {
            Some(prev_route) => {
                if match job.sat_direction {
                    SatDirection::Pull => {
                        candidatelist.contains(&prev_route.first().unwrap().channel)
                    }
                    SatDirection::Push => {
                        candidatelist.contains(&prev_route.last().unwrap().channel)
                    }
                } {
                    route = prev_route
                } else {
                    success_route = None;
                    continue;
                }
            }
            None => {
                let slingchan_inc = graph.get_channel(other_peer, chan_id)?;
                let slingchan_out = graph.get_channel(mypubkey, chan_id)?;

                let maxhops = match job.maxhops {
                    Some(h) => h + 1,
                    None => 9,
                };
                let mut hops = 3;

                let mut pull_jobs = plugin.state().pull_jobs.lock().clone();
                let mut push_jobs = plugin.state().push_jobs.lock().clone();
                let excepts = plugin.state().excepts.lock().clone();
                for except in &excepts {
                    pull_jobs.insert(except.to_string());
                    push_jobs.insert(except.to_string());
                }
                while (route.len() == 0
                    || route.len() > 0
                        && feeppm_effective_from_amts(
                            Amount::msat(&route.first().unwrap().amount_msat),
                            Amount::msat(&route.last().unwrap().amount_msat),
                        ) > job.maxppm)
                    && hops <= maxhops
                {
                    match job.sat_direction {
                        SatDirection::Pull => {
                            route = dijkstra(
                                &mypubkey,
                                &graph,
                                mypubkey,
                                other_peer,
                                DijkstraNode {
                                    score: 0,
                                    destination: mypubkey,
                                    channel: slingchan_inc.channel.clone(),
                                    hops: 0,
                                },
                                &job,
                                &candidatelist,
                                &pull_jobs,
                                hops,
                            )?;
                        }
                        SatDirection::Push => {
                            route = dijkstra(
                                &mypubkey,
                                &graph,
                                other_peer,
                                mypubkey,
                                DijkstraNode {
                                    score: 0,
                                    destination: other_peer,
                                    channel: slingchan_out.channel.clone(),
                                    hops: 0,
                                },
                                &job,
                                &candidatelist,
                                &push_jobs,
                                hops,
                            )?;
                        }
                    }
                    hops += 1;
                }
            }
        }

        if route.len() == 0 {
            info!(
                "{}: could not find a route. Sleeping...",
                chan_id.to_string()
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                JobMessage::NoRoute,
                None,
                None,
            );
            success_route = None;
            time::sleep(Duration::from_secs(10)).await;
            continue 'outer;
        }

        if feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        ) > job.maxppm
        {
            info!(
                "{}: no cheap enough route found! Sleeping...",
                chan_id.to_string()
            );
            channel_jobstate_update(
                plugin.state().job_state.clone(),
                chan_id,
                JobMessage::TooExp,
                None,
                None,
            );
            time::sleep(Duration::from_secs(120)).await;
            success_route = None;
            continue 'outer;
        }

        info!(
            "{}: Found route with {} hops. Total: {}ms",
            chan_id.to_string(),
            route.len() - 1,
            now.elapsed().as_millis().to_string()
        );

        let alias_map = plugin.state().alias_peer_map.lock().clone();
        for r in &route {
            debug!(
                "{}: route: {} {:3} {:17} {}",
                chan_id.to_string(),
                Amount::msat(&r.amount_msat),
                r.delay,
                r.channel.to_string(),
                alias_map.get(&r.id).unwrap_or(&r.id.to_string()),
            );
        }

        let (preimage, payment_hash) = get_preimage_paymend_hash_pair();
        debug!(
            "{}: Made preimage and payment_hash: {} Total: {}ms",
            chan_id.to_string(),
            payment_hash.to_string(),
            now.elapsed().as_millis().to_string()
        );

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
                        "{}: First peer not ready, banning it for now...",
                        chan_id.to_string()
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
                        JobMessage::Error,
                        None,
                        Some(false),
                    );
                    warn!(
                        "{}: Unexpected sendpay error: {}",
                        chan_id.to_string(),
                        e.to_string()
                    );
                    break 'outer;
                }
            }
        };
        info!(
            "{}: Sent on route. Total: {}ms",
            chan_id.to_string(),
            now.elapsed().as_millis().to_string()
        );

        match waitsendpay(&networkdir, send_response.payment_hash, None, None).await {
            Ok(o) => {
                info!(
                    "{}: Rebalance SUCCESSFULL after {}s. Sent {}sats plus {}msats fee",
                    chan_id.to_string(),
                    now.elapsed().as_secs().to_string(),
                    Amount::msat(&o.amount_msat.unwrap()) / 1_000,
                    Amount::msat(&o.amount_sent_msat) - Amount::msat(&o.amount_msat.unwrap()),
                );
                delpay(&networkdir, send_response.payment_hash, "complete").await?;
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
                                "{}: Rebalance WAITSENDPAY_TIMEOUT failure after {}s: {}",
                                chan_id.to_string(),
                                now.elapsed().as_secs().to_string(),
                                e.message,
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
                                    info!(
                                        "{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                                        chan_id.to_string(),
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
                                                "{}: PAYMENT DETAILS ERROR:{:?} {:?}",
                                                chan_id.to_string(),
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
                                            JobMessage::Error,
                                            None,
                                            Some(false),
                                        );
                                        warn!(
                                            "{}: UNEXPECTED waitsendpay failure after {}s: {:?}",
                                            chan_id.to_string(),
                                            now.elapsed().as_secs().to_string(),
                                            e
                                        );
                                        break 'outer;
                                    }

                                    if data.erring_channel == route.last().unwrap().channel {
                                        warn!(
                                            "{}: Peer has a problem or just updated their fees? {}",
                                            chan_id.to_string(),
                                            data.failcodename
                                        );
                                        time::sleep(Duration::from_secs(20)).await;
                                    } else {
                                        debug!(
                                            "{}: Adjusting liquidity for {}.",
                                            chan_id.to_string(),
                                            data.erring_channel.to_string()
                                        );
                                        plugin
                                            .state()
                                            .graph
                                            .lock()
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
                                        JobMessage::Error,
                                        None,
                                        Some(false),
                                    );
                                    warn!(
                                        "{}: UNEXPECTED waitsendpay failure after {}s: {:?}",
                                        chan_id.to_string(),
                                        now.elapsed().as_secs().to_string(),
                                        e
                                    );
                                    break 'outer;
                                }
                            };
                        }
                        let delpay_timer = Instant::now();
                        while delpay_timer.elapsed() < Duration::from_secs(120) {
                            match delpay(&networkdir, send_response.payment_hash, "failed").await {
                                Ok(_) => break,
                                Err(_) => {
                                    time::sleep(Duration::from_secs(1)).await;
                                    continue;
                                }
                            }
                        }
                    }
                    None => (),
                }
            }
        };
    }

    Ok(())
}

async fn health_check(
    chan_id: ShortChannelId,
    job: &Job,
    peers: &Vec<ListpeersPeers>,
    other_peer: PublicKey,
    job_states: Arc<Mutex<HashMap<String, JobState>>>,
) -> Option<bool> {
    let our_listpeers_channel = get_normal_channel_from_listpeers(peers, chan_id);
    if let Some(channel) = our_listpeers_channel {
        if job.is_balanced(&channel, &chan_id)
            || match job.sat_direction {
                SatDirection::Pull => Amount::msat(&channel.receivable_msat.unwrap()) < job.amount,
                SatDirection::Push => Amount::msat(&channel.spendable_msat.unwrap()) < job.amount,
            }
        {
            info!(
                "{}: already balanced. Taking a break...",
                chan_id.to_string()
            );
            channel_jobstate_update(
                job_states.clone(),
                chan_id,
                JobMessage::Balanced,
                None,
                None,
            );
            time::sleep(Duration::from_secs(120)).await;
            Some(true)
        } else if match job.sat_direction {
            SatDirection::Pull => false,
            SatDirection::Push => get_out_htlc_count(&channel) > 5,
        } {
            info!(
                "{}: already more than 5 pending htlcs. Taking a break...",
                chan_id.to_string()
            );
            channel_jobstate_update(
                job_states.clone(),
                chan_id,
                JobMessage::HTLCcapped,
                None,
                None,
            );
            time::sleep(Duration::from_secs(120)).await;
            Some(true)
        } else {
            match peers.iter().find(|x| x.id == other_peer) {
                Some(p) => {
                    if !p.connected {
                        info!("{}: not connected. Taking a break...", chan_id.to_string());
                        channel_jobstate_update(
                            job_states.clone(),
                            chan_id,
                            JobMessage::Disconnected,
                            None,
                            None,
                        );
                        time::sleep(Duration::from_secs(120)).await;
                        Some(true)
                    } else {
                        None
                    }
                }
                None => {
                    channel_jobstate_update(
                        job_states.clone(),
                        chan_id,
                        JobMessage::PeerNotFound,
                        Some(false),
                        None,
                    );
                    warn!("{}: peer not found. Stopping job.", chan_id.to_string());
                    Some(false)
                }
            }
        }
    } else {
        warn!(
            "{}: not in CHANNELD_NORMAL state. Stopping Job.",
            chan_id.to_string()
        );
        channel_jobstate_update(
            job_states.clone(),
            chan_id,
            JobMessage::ChanNotNormal,
            Some(false),
            None,
        );
        Some(false)
    }
}

fn build_candidatelist(
    peers: &Vec<ListpeersPeers>,
    job: &Job,
    graph: &LnGraph,
    tempbans: HashMap<String, u64>,
) -> Vec<ShortChannelId> {
    let mut candidatelist = Vec::<ShortChannelId>::new();
    for peer in peers {
        for channel in &peer.channels {
            if let Some(scid) = channel.short_channel_id {
                if match channel.state {
                    ListpeersPeersChannelsState::CHANNELD_NORMAL => true,
                    _ => false,
                } && peer.connected
                {
                    let chan_from_peer = match graph.get_channel(peer.id, scid) {
                        Ok(chan) => chan.channel,
                        Err(_) => continue,
                    };

                    let to_us_msat = Amount::msat(&channel.to_us_msat.unwrap());
                    let total_msat = Amount::msat(&channel.total_msat.unwrap());

                    if match job.sat_direction {
                        SatDirection::Pull => {
                            feeppm_effective(
                                channel.fee_proportional_millionths.unwrap(),
                                Amount::msat(&channel.fee_base_msat.unwrap()) as u32,
                                job.amount,
                            ) <= job.outppm as u64
                                && to_us_msat > (0.2 * total_msat as f64) as u64
                                && get_out_htlc_count(channel) < 5
                        }
                        SatDirection::Push => {
                            feeppm_effective(
                                chan_from_peer.fee_per_millionth,
                                chan_from_peer.base_fee_millisatoshi,
                                job.amount,
                            ) >= job.outppm as u64
                                && total_msat - to_us_msat > (0.2 * total_msat as f64) as u64
                                && get_in_htlc_count(channel) < 5
                        }
                    } && !tempbans.contains_key(&scid.to_string())
                    {
                        candidatelist.push(scid.clone());
                    }
                }
            }
        }
    }

    candidatelist
}
