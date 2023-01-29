use anyhow::{anyhow, Error};

use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::{Hash, HashEngine};
use cln_plugin::Plugin;

use cln_rpc::{model::*, primitives::*};

use log::{debug, info, warn};

use rand::{thread_rng, Rng};

use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::{BinaryHeap, HashSet};

use std::path::Path;
use std::time::SystemTime;
use std::{
    collections::HashMap,
    time::{Duration, UNIX_EPOCH},
};
use std::{path::PathBuf, str::FromStr};

use tokio::time::{self, Instant};

use crate::model::{DijkstraNode, FailureReb, Job, JobMessage, LnGraph, SatDirection, SuccessReb};
use crate::{
    delpay, get_normal_channel_from_listpeers, scored::*, slingsend, waitsendpay, PluginState,
    PLUGIN_NAME,
};

pub async fn sling(
    rpc_path: &PathBuf,
    chan_id: ShortChannelId,
    job: Job,
    plugin: &Plugin<PluginState>,
) -> Result<(), Error> {
    let config = plugin.state().config.lock().clone();
    let mypubkey = config.pubkey.unwrap().clone();

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
            let mut jobstates = plugin.state().job_state.lock();
            let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
            jobstate.statechange(JobMessage::Stopped);
            jobstate.set_active(false);
            break;
        }

        let peers = plugin.state().peers.lock().clone();

        let our_listpeers_channel = get_normal_channel_from_listpeers(&peers, &chan_id);

        let other_peer = get_peer_id_from_chan_id(&peers, chan_id)?;

        if let Some(channel) = our_listpeers_channel {
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
                    "{}: already balanced. Taking a break...",
                    chan_id.to_string()
                );
                plugin
                    .state()
                    .job_state
                    .lock()
                    .get_mut(&chan_id.to_string())
                    .unwrap()
                    .statechange(JobMessage::Balanced);
                time::sleep(Duration::from_secs(120)).await;
                continue 'outer;
            } else if match job.sat_direction {
                SatDirection::Pull => false,
                SatDirection::Push => get_out_htlc_count(&channel) > 5,
            } {
                info!(
                    "{}: already more than 5 pending htlcs. Taking a break...",
                    chan_id.to_string()
                );
                plugin
                    .state()
                    .job_state
                    .lock()
                    .get_mut(&chan_id.to_string())
                    .unwrap()
                    .statechange(JobMessage::HTLCcapped);
                time::sleep(Duration::from_secs(600)).await;
                continue 'outer;
            } else {
                match peers.iter().find(|x| x.id == other_peer) {
                    Some(p) => {
                        if !p.connected {
                            info!("{}: not connected. Taking a break...", chan_id.to_string());
                            plugin
                                .state()
                                .job_state
                                .lock()
                                .get_mut(&chan_id.to_string())
                                .unwrap()
                                .statechange(JobMessage::Disconnected);
                            time::sleep(Duration::from_secs(600)).await;
                            continue 'outer;
                        }
                    }
                    None => {
                        let mut jobstates = plugin.state().job_state.lock();
                        let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
                        jobstate.statechange(JobMessage::PeerNotFound);
                        jobstate.set_active(false);

                        warn!("{}: peer not found. Stopping job.", chan_id.to_string());
                        break 'outer;
                    }
                }
            }
        } else {
            warn!(
                "{}: not in CHANNELD_NORMAL state. Stopping Job.",
                chan_id.to_string()
            );
            let mut jobstates = plugin.state().job_state.lock();
            let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
            jobstate.statechange(JobMessage::ChanNotNormal);
            jobstate.set_active(false);

            break 'outer;
        }

        let graph = plugin.state().graph.lock().clone();

        if graph.graph.len() == 0 {
            info!("{}: graph is still empty. Sleeping...", chan_id.to_string());
            plugin
                .state()
                .job_state
                .lock()
                .get_mut(&chan_id.to_string())
                .unwrap()
                .statechange(JobMessage::GraphEmpty);

            time::sleep(Duration::from_secs(10)).await;
            continue 'outer;
        }
        plugin
            .state()
            .job_state
            .lock()
            .get_mut(&chan_id.to_string())
            .unwrap()
            .statechange(JobMessage::Rebalancing);
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
        if candidatelist.len() == 0 {
            info!(
                "{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
                chan_id.to_string()
            );
            plugin
                .state()
                .job_state
                .lock()
                .get_mut(&chan_id.to_string())
                .unwrap()
                .statechange(JobMessage::NoCandidates);
            time::sleep(Duration::from_secs(10)).await;
            continue 'outer;
        }

        let mut route = Vec::new();
        match success_route {
            Some(e) => {
                if match job.sat_direction {
                    SatDirection::Pull => !candidatelist.contains(&e.first().unwrap().channel),
                    SatDirection::Push => !candidatelist.contains(&e.last().unwrap().channel),
                } {
                    success_route = None;
                    continue;
                } else {
                    route = e
                }
            }
            None => {
                let slingchan_inc = graph.get_channel(other_peer, chan_id)?;
                let slingchan_out = graph.get_channel(mypubkey, chan_id)?;

                let maxhops = match job.maxhops {
                    Some(h) => h,
                    None => 8,
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
        debug!(
            "{}: Found route. Total: {}ms",
            chan_id.to_string(),
            now.elapsed().as_millis().to_string()
        );
        if route.len() == 0 {
            info!(
                "{}: could not find a route. Sleeping...",
                chan_id.to_string()
            );
            plugin
                .state()
                .job_state
                .lock()
                .get_mut(&chan_id.to_string())
                .unwrap()
                .statechange(JobMessage::NoRoute);

            success_route = None;
            time::sleep(Duration::from_secs(10)).await;
            continue 'outer;
        }

        let alias_map = plugin.state().alias_peer_map.lock().clone();
        for r in &route {
            debug!(
                "{} found route: {} {} {} {}",
                chan_id.to_string(),
                Amount::msat(&r.amount_msat),
                r.delay,
                alias_map.get(&r.id).unwrap_or(&r.id.to_string()),
                r.channel.to_string()
            );
        }
        let prettyroute = route
            .iter()
            .map(|x| x.channel.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
            + " NODES: "
            + &route
                .iter()
                .map(|x| alias_map.get(&x.id).unwrap_or(&x.id.to_string()).clone())
                .collect::<Vec<_>>()
                .join(" -> ");
        let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
        debug!(
            "{}: Made pretty route. Total: {}ms",
            chan_id.to_string(),
            now.elapsed().as_millis().to_string()
        );
        let send_response;
        if feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        ) <= job.maxppm
        {
            let mut preimage = [0u8; 32];
            thread_rng().fill(&mut preimage[..]);

            let pi_str = serialize_hex(&preimage);

            let mut hasher = Sha256::engine();
            hasher.input(&preimage);
            let payment_hash = Sha256::from_engine(hasher);
            debug!(
                "{}: Made payment hash: {} Total: {}ms",
                chan_id.to_string(),
                payment_hash.to_string(),
                now.elapsed().as_millis().to_string()
            );

            match slingsend(&rpc_path, route.clone(), payment_hash, None, None).await {
                Ok(resp) => {
                    plugin
                        .state()
                        .pays
                        .write()
                        .insert(payment_hash.to_string(), pi_str);
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
                        // delinvoice(&rpc_path, label, DelinvoiceStatus::UNPAID).await?;
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
                        let mut jobstates = plugin.state().job_state.lock();
                        let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
                        jobstate.statechange(JobMessage::Error);
                        jobstate.set_active(false);

                        warn!(
                            "{}: Unexpected sendpay error: {}",
                            chan_id.to_string(),
                            e.to_string()
                        );
                        break 'outer;
                    }
                }
            };
            debug!(
                "{}: Sent on route. Total: {}ms",
                chan_id.to_string(),
                now.elapsed().as_millis().to_string()
            );
        } else {
            info!(
                "{}: no cheap enough route found! Sleeping...",
                chan_id.to_string()
            );
            {
                let mut jobstates = plugin.state().job_state.lock();
                let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
                jobstate.statechange(JobMessage::NoRoute);
            }
            time::sleep(Duration::from_secs(600)).await;
            success_route = None;
            continue;
        }
        let mut networkdir =
            PathBuf::from_str(&plugin.configuration().lightning_dir.clone()).unwrap();
        networkdir.pop();
        // let waitsendpay_timeout_start = Instant::now();
        // while waitsendpay_timeout_start.elapsed() < Duration::from_secs(120) {
        //     match waitsendpay(&networkdir, send_response.payment_hash, None, None).await {
        //         Ok(_o) => break,
        //         Err(e) => match e.code {
        //             Some(c) => match c {
        //                 208 => {
        //                     time::sleep(Duration::from_secs(1)).await;
        //                     continue;
        //                 }
        //                 204 => break,
        //                 _ => {
        //                     time::sleep(Duration::from_secs(1)).await;
        //                     continue;
        //                 }
        //             },
        //             None => {
        //                 time::sleep(Duration::from_secs(1)).await;
        //                 continue;
        //             }
        //         },
        //     }
        // }

        match waitsendpay(&networkdir, send_response.payment_hash, None, None).await {
            Ok(o) => {
                info!(
                    "{}: Rebalance successfull after {}s. Sent {}sats plus {}msats as fee over CHANNELS: {}",
                    chan_id.to_string(),
                    now.elapsed().as_secs().to_string(),
                    Amount::msat(&o.amount_msat.unwrap()) / 1_000,
                    Amount::msat(&o.amount_sent_msat) - Amount::msat(&o.amount_msat.unwrap()),
                    prettyroute
                );
                // delinvoice(&rpc_path, o.label.unwrap(), DelinvoiceStatus::PAID).await?;
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
                    .remove(&send_response.payment_hash);
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
                                        let mut jobstates = plugin.state().job_state.lock();
                                        let jobstate =
                                            jobstates.get_mut(&chan_id.to_string()).unwrap();
                                        jobstate.statechange(JobMessage::Error);
                                        jobstate.set_active(false);

                                        warn!(
                                            "{}: UNEXPECTED waitsendpay failure after {}s: {:?}",
                                            chan_id.to_string(),
                                            now.elapsed().as_secs().to_string(),
                                            e
                                        );
                                        break 'outer;
                                    }
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
                                                    Amount::msat(&data.amount_msat.unwrap()) - 1;
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
                                None => {
                                    let mut jobstates = plugin.state().job_state.lock();
                                    let jobstate = jobstates.get_mut(&chan_id.to_string()).unwrap();
                                    jobstate.statechange(JobMessage::Error);
                                    jobstate.set_active(false);

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

fn get_out_htlc_count(channel: &ListpeersPeersChannels) -> u64 {
    match &channel.htlcs {
        Some(htlcs) => htlcs
            .into_iter()
            .filter(|htlc| match htlc.direction {
                ListpeersPeersChannelsHtlcsDirection::OUT => true,
                ListpeersPeersChannelsHtlcsDirection::IN => false,
            })
            .count() as u64,
        None => 0,
    }
}

fn get_peer_id_from_chan_id(
    peers: &Vec<ListpeersPeers>,
    channel: ShortChannelId,
) -> Result<PublicKey, Error> {
    let now = Instant::now();
    let peer = peers.iter().find(|peer| {
        peer.channels
            .iter()
            .any(|chan| chan.short_channel_id == Some(channel))
    });
    match peer {
        Some(p) => Ok(p.id),
        None => {
            debug!(
                "get_peer_id_from_chan_id in {}ms",
                now.elapsed().as_millis().to_string()
            );
            Err(anyhow!("{} disappeard from peers", channel.to_string()))
        }
    }
}

fn dijkstra(
    mypubkey: &PublicKey,
    lngraph: &LnGraph,
    start: PublicKey,
    goal: PublicKey,
    slingchan: DijkstraNode,
    job: &Job,
    candidatelist: &Vec<ShortChannelId>,
    exclude: &HashSet<String>,
    hops: u8,
) -> Result<Vec<SendpayRoute>, Error> {
    let mut visited = HashSet::with_capacity(lngraph.graph.len());
    let mut scores = HashMap::new();
    let mut predecessor = HashMap::new();
    let mut visit_next = BinaryHeap::new();
    let zero_score = u64::default();
    scores.insert(start, slingchan.clone());
    visit_next.push(MinScored(zero_score, start));
    while let Some(MinScored(node_score, node)) = visit_next.pop() {
        if visited.contains(&node) {
            // debug!(
            //     "{}: already visited: {}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &node
            // );
            continue;
        }
        if &goal == &node {
            // debug!(
            //     "{}: arrived at goal: {}  {}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &goal,
            //     &node
            // );
            break;
        }
        let current_hops = scores.get(&node).unwrap().hops;
        if current_hops + 2 > hops {
            continue;
        }
        for edge in lngraph.edges(mypubkey, &node, exclude, &job.amount, &candidatelist) {
            let next = edge.channel.destination;
            if visited.contains(&next) {
                // debug!(
                //     "{}: already visited: {}",
                //     slingchan.channel.short_channel_id.to_string(),
                //     &next
                // );
                continue;
            }
            let next_score = if edge.channel.source == *mypubkey {
                0
            } else {
                node_score + edge_cost(&edge.channel, job.amount)
            };
            // debug!(
            //     "{}: next: {} node_score:{} next_score:{}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &next,
            //     &node_score,
            //     &next_score
            // );
            let dijkstra_node = DijkstraNode {
                score: next_score,
                channel: edge.channel.clone(),
                destination: next.clone(),
                hops: current_hops + 1,
            };
            match scores.entry(next) {
                Occupied(ent) => {
                    if next_score < ent.get().score {
                        // debug!(
                        //     "{}: found better path to: {}",
                        //     slingchan.channel.short_channel_id.to_string(),
                        //     &next
                        // );
                        *ent.into_mut() = dijkstra_node;
                        visit_next.push(MinScored(next_score, next));
                        predecessor.insert(next.clone(), node.clone());
                    }
                }
                Vacant(ent) => {
                    // debug!(
                    //     "{}: found new path to: {} via {}",
                    //     slingchan.channel.short_channel_id.to_string(),
                    //     &next,
                    //     &edge.channel.short_channel_id.to_string()
                    // );
                    ent.insert(dijkstra_node);
                    visit_next.push(MinScored(next_score, next));
                    predecessor.insert(next.clone(), node.clone());
                }
            }
        }
        visited.insert(node);
    }

    //TODO remove this line
    // route_fix.get_mut(0).unwrap().amount_msat = route_fix.get(1).unwrap().amount_msat.clone();
    Ok(build_route(
        &predecessor,
        &goal,
        &scores,
        job,
        &start,
        &slingchan,
    )?)
}

fn build_route(
    predecessor: &HashMap<PublicKey, PublicKey>,
    goal: &PublicKey,
    scores: &HashMap<PublicKey, DijkstraNode>,
    job: &Job,
    start: &PublicKey,
    slingchan: &DijkstraNode,
) -> Result<Vec<SendpayRoute>, Error> {
    let mut dijkstra_path = Vec::new();
    // debug!("predecssors: {:?}", predecessor);
    let mut prev;
    match predecessor.get(&goal) {
        Some(node) => prev = node,
        None => return Ok(vec![]),
    };
    dijkstra_path.push(scores.get(&goal).unwrap().clone());
    // debug!(
    //     "{}: found potential new route with #hops: {}",
    //     slingchan.channel.short_channel_id.to_string(),
    //     dijkstra_path.get(0).unwrap().hops
    // );

    while prev != start {
        let spr = scores.get(&prev).unwrap().clone();
        prev = predecessor.get(&prev).unwrap();
        dijkstra_path.push(spr);
    }
    match job.sat_direction {
        SatDirection::Pull => dijkstra_path.insert(0, slingchan.clone()),
        SatDirection::Push => dijkstra_path.push(slingchan.clone()),
    }

    let mut sendpay_route = Vec::new();
    let mut prev_amount_msat;
    let mut amount_msat = Amount::from_msat(0);
    let mut delay = 20;

    // dijkstra_path.remove(0);
    // let mut i = 0;
    for hop in &dijkstra_path {
        if hop == dijkstra_path.first().unwrap() {
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat: Amount::from_msat(job.amount),
                    id: dijkstra_path.get(0).unwrap().destination,
                    delay,
                    channel: dijkstra_path.get(0).unwrap().channel.short_channel_id,
                },
            );
        } else {
            // let delay = prev.delay + hop.channel.delay as u16;
            // let amount_msat = Amount::from_msat(
            //     Amount::msat(&prev.amount_msat)
            //         + fee_total_msat_precise(
            //             hop.channel.fee_per_millionth,
            //             hop.channel.base_fee_millisatoshi,
            //             Amount::msat(&prev.amount_msat),
            //         )
            //         .ceil() as u64,
            // );
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat,
                    id: hop.destination,
                    delay,
                    channel: hop.channel.short_channel_id,
                },
            );
            // i += 1;
        }
        prev_amount_msat = sendpay_route.get(0).unwrap().amount_msat;
        amount_msat = Amount::from_msat(
            Amount::msat(&prev_amount_msat)
                + fee_total_msat_precise(
                    hop.channel.fee_per_millionth,
                    hop.channel.base_fee_millisatoshi,
                    Amount::msat(&prev_amount_msat),
                )
                .ceil() as u64,
        );
        delay = delay + hop.channel.delay as u16;
    }
    Ok(sendpay_route)
}

pub fn edge_cost(edge: &ListchannelsChannels, amount: u64) -> u64 {
    // debug!(
    //     "edge cost for {} source:{} is {}",
    //     edge.short_channel_id.to_string(),
    //     edge.source,
    //     (edge.base_fee_millisatoshi as f64
    //         + edge.fee_per_millionth as f64 / 1_000_000.0 * amount as f64) as u64
    // );
    fee_total_msat_precise(edge.fee_per_millionth, edge.base_fee_millisatoshi, amount).ceil() as u64
}

pub fn feeppm_effective(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> u64 {
    (fee_total_msat_precise(feeppm, basefee_msat, amount_msat) / amount_msat as f64 * 1_000_000.0)
        .ceil() as u64
}

pub fn fee_total_msat_precise(feeppm: u32, basefee_msat: u32, amount_msat: u64) -> f64 {
    basefee_msat as f64 + (feeppm as f64 / 1_000_000.0 * amount_msat as f64)
}

pub fn feeppm_effective_from_amts(amount_msat_start: u64, amount_msat_end: u64) -> u32 {
    if amount_msat_start < amount_msat_end {
        panic!(
            "CRITICAL ERROR: amount_msat_start should be greater than or equal to amount_msat_end"
        )
    }
    ((amount_msat_start - amount_msat_end) as f64 / amount_msat_end as f64 * 1_000_000.0).ceil()
        as u32
}
