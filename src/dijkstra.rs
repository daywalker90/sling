use std::{
    cmp::Ordering,
    collections::{
        BinaryHeap,
        HashMap,
        HashSet,
        hash_map::Entry::{Occupied, Vacant},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Error, anyhow};
use cln_rpc::{
    model::requests::SendpayRoute,
    primitives::{Amount, ShortChannelId, ShortChannelIdDir},
};
use sling::{Job, SatDirection};

use crate::{
    model::{
        Config,
        DijkstraNode,
        JobMessage,
        Liquidity,
        LnGraph,
        MAX_ROUTE_CLTV_DELTA,
        PubKeyBytes,
        Task,
    },
    util::{div_ceil_i64, edge_cost},
};
pub fn dijkstra(
    config: &Config,
    lngraph: &LnGraph,
    job: &Job,
    task: &mut Task,
    actual_candidates: &[ShortChannelId],
    excepts: &[ShortChannelIdDir],
    liquidity: &HashMap<ShortChannelIdDir, Liquidity>,
) -> Result<Vec<SendpayRoute>, Error> {
    let two_weeks_ago = u32::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 60 * 60 * 24 * 14,
    )?;

    let mut visited = HashSet::with_capacity(lngraph.node_count());
    let mut scores = HashMap::new();
    let mut predecessor = HashMap::new();
    let mut visit_next = BinaryHeap::new();
    let zero_score = u64::default();

    let (start, goal) = match job.sat_direction {
        SatDirection::Pull => (config.pubkey_bytes, task.other_pubkey),
        SatDirection::Push => (task.other_pubkey, config.pubkey_bytes),
    };
    let slingchan = construct_first_node(task, config, lngraph, job.sat_direction, start)?;
    scores.insert(&start, slingchan);
    visit_next.push(MinScored(zero_score, &start));
    while let Some(MinScored(node_score, node)) = visit_next.pop() {
        if visited.contains(&node) {
            // debug!(
            //     "{}: already visited: {}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &node
            // );
            continue;
        }
        if &goal == node {
            // debug!(
            //     "{}: arrived at goal: {}  {}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &goal,
            //     &node
            // );
            break;
        }
        let current_node = *scores.get(&node).unwrap();
        let current_hops = current_node.hops;
        if current_hops + 2 > job.get_maxhops(config.maxhops) {
            continue;
        }
        // Inbound fee is charged by `node` on the channel it received the HTLC from.
        let node_inbound_fee = if *node == config.pubkey_bytes {
            None
        } else {
            lngraph
                .get_state_direction(ShortChannelIdDir {
                    short_channel_id: current_node.short_channel_id_dir.short_channel_id,
                    direction: 1 - current_node.short_channel_id_dir.direction,
                })
                .and_then(|s| s.inbound_fee)
        };
        for (scid, edge) in lngraph.edges(
            node,
            two_weeks_ago,
            actual_candidates,
            config,
            job,
            excepts,
            liquidity,
        ) {
            if visited.contains(&edge.destination) {
                // debug!(
                //     "{}: already visited: {}",
                //     slingchan.channel.short_channel_id.to_string(),
                //     &next
                // );
                continue;
            }
            let next_delay = if edge.source == config.pubkey_bytes {
                current_node.delay
            } else {
                current_node.delay.saturating_add(edge.delay)
            };
            if next_delay > MAX_ROUTE_CLTV_DELTA {
                continue;
            }
            let mut next_score = if edge.source == config.pubkey_bytes {
                0
            } else {
                let out_cost = edge_cost(edge, node_inbound_fee, job.amount_msat);
                node_score.saturating_add(out_cost)
            };
            if job.sat_direction == SatDirection::Pull && edge.destination == goal {
                let goal_inbound_fee = lngraph
                    .get_state_direction(ShortChannelIdDir {
                        short_channel_id: scid.short_channel_id,
                        direction: 1 - scid.direction,
                    })
                    .and_then(|s| s.inbound_fee);
                let goal_cost = edge_cost(&slingchan.channel_state, goal_inbound_fee, job.amount_msat);
                next_score = next_score.saturating_add(goal_cost);
            }
            // debug!(
            //     "{}: next: {} node_score:{} next_score:{}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &next,
            //     &node_score,
            //     &next_score
            // );

            match scores.entry(&edge.destination) {
                Occupied(ent) => {
                    if next_score < ent.get().score {
                        // debug!(
                        //     "{}: found better path to: {}",
                        //     slingchan.channel.short_channel_id.to_string(),
                        //     &next
                        // );
                        let dijkstra_node = DijkstraNode {
                            score: next_score,
                            channel_state: *edge,
                            destination: edge.destination,
                            hops: current_hops + 1,
                            short_channel_id_dir: *scid,
                            delay: next_delay,
                        };
                        *ent.into_mut() = dijkstra_node;
                        visit_next.push(MinScored(next_score, &edge.destination));
                        predecessor.insert(&edge.destination, node);
                    }
                }
                Vacant(ent) => {
                    // debug!(
                    //     "{}: found new path to: {} via {}",
                    //     slingchan.channel.short_channel_id.to_string(),
                    //     &next,
                    //     &edge.channel.short_channel_id.to_string()
                    // );
                    let dijkstra_node = DijkstraNode {
                        score: next_score,
                        channel_state: *edge,
                        destination: edge.destination,
                        hops: current_hops + 1,
                        short_channel_id_dir: *scid,
                        delay: next_delay,
                    };
                    ent.insert(dijkstra_node);
                    visit_next.push(MinScored(next_score, &edge.destination));
                    predecessor.insert(&edge.destination, node);
                }
            }
        }
        visited.insert(node);
    }

    Ok(build_route(
        lngraph,
        &predecessor,
        &goal,
        &scores,
        job,
        &start,
        &slingchan,
        config,
    ))
}

fn build_route(
    lngraph: &LnGraph,
    predecessor: &HashMap<&PubKeyBytes, &PubKeyBytes>,
    goal: &PubKeyBytes,
    scores: &HashMap<&PubKeyBytes, DijkstraNode>,
    job: &Job,
    start: &PubKeyBytes,
    slingchan: &DijkstraNode,
    config: &Config,
) -> Vec<SendpayRoute> {
    let mut dijkstra_path = Vec::new();
    // debug!("predecssors: {:?}", predecessor);
    let mut prev;
    match predecessor.get(goal) {
        Some(node) => prev = *node,
        None => return vec![],
    }
    dijkstra_path.push(*scores.get(goal).unwrap());
    // debug!(
    //     "{}: found potential new route with #hops: {}",
    //     slingchan.channel.short_channel_id.to_string(),
    //     dijkstra_path.get(0).unwrap().hops
    // );

    while prev != start {
        let spr = scores.get(prev).unwrap();

        prev = predecessor.get(prev).unwrap();
        dijkstra_path.push(*spr);
    }
    match job.sat_direction {
        SatDirection::Pull => dijkstra_path.insert(0, *slingchan),
        SatDirection::Push => dijkstra_path.push(*slingchan),
    }

    let mut sendpay_route = Vec::new();
    let mut prev_amount_msat;
    let mut amount_msat = Amount::from_msat(0);
    let mut delay = std::cmp::max(config.cltv_delta, config.cltv_final);

    let first_hop = if dijkstra_path.is_empty() {
        return sendpay_route;
    } else {
        dijkstra_path.first().unwrap()
    };

    for (i, hop) in dijkstra_path.iter().enumerate() {
        if hop == first_hop {
            let routing_scid = if let Some(rscid) = first_hop.channel_state.scid_alias {
                rscid
            } else {
                first_hop.short_channel_id_dir.short_channel_id
            };
            let Ok(dest_pubkey) = dijkstra_path.first().unwrap().destination.try_to_pubkey() else {
                return vec![];
            };
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat: Some(Amount::from_msat(job.amount_msat)),
                    id: Some(dest_pubkey),
                    delay: Some(delay),
                    channel: Some(routing_scid),
                    amount_out_msat: None,
                    cltv_out: None,
                    node_id_out: None,
                    short_channel_id_dir: None,
                },
            );
        } else {
            let routing_scid = if let Some(rscid) = hop.channel_state.scid_alias {
                rscid
            } else {
                hop.short_channel_id_dir.short_channel_id
            };
            let Ok(dest_pubkey) = hop.destination.try_to_pubkey() else {
                return vec![];
            };
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat: Some(amount_msat),
                    id: Some(dest_pubkey),
                    delay: Some(delay),
                    channel: Some(routing_scid),
                    amount_out_msat: None,
                    cltv_out: None,
                    node_id_out: None,
                    short_channel_id_dir: None,
                },
            );
        }
        prev_amount_msat = sendpay_route.first().unwrap().amount_msat.unwrap();

        if i + 1 < dijkstra_path.len() {
            let prev_amt = prev_amount_msat.msat();
            let Some(outbound_fee_msat) = prev_amt
                .checked_mul(u64::from(hop.channel_state.fee_per_millionth))
                .map(|p| p.div_ceil(1_000_000))
                .and_then(|f| f.checked_add(u64::from(hop.channel_state.base_fee_millisatoshi)))
            else {
                return vec![];
            };

            let Some(net_received_amt) = prev_amt.checked_add(outbound_fee_msat) else {
                return vec![];
            };

            // inbound fee is charged by the *next* hop's node, so look ahead
            let inbound_fee_msat: i64 = if let Some(next_hop) = dijkstra_path.get(i + 1) {
                let opposite_dir = ShortChannelIdDir {
                    short_channel_id: next_hop.short_channel_id_dir.short_channel_id,
                    direction: 1 - next_hop.short_channel_id_dir.direction,
                };
                if let Some(inf) = lngraph
                    .get_state_direction(opposite_dir)
                    .and_then(|s| s.inbound_fee)
                {
                    let Ok(net_i64) = i64::try_from(net_received_amt) else {
                        return vec![];
                    };
                    let Some(prop_fee) = net_i64
                        .checked_mul(i64::from(inf.proportional_millionths))
                        .map(|p| div_ceil_i64(p, 1_000_000))
                    else {
                        return vec![];
                    };
                    let Some(fee) = prop_fee.checked_add(i64::from(inf.base_msat)) else {
                        return vec![];
                    };
                    log::debug!("{opposite_dir}: inbound fee: {fee} msat");
                    fee
                } else {
                    0
                }
            } else {
                0
            };

            let next_amt = if inbound_fee_msat >= 0 {
                let Some(amt) = net_received_amt.checked_add(inbound_fee_msat as u64) else {
                    return vec![];
                };
                amt
            } else {
                net_received_amt
                    .saturating_sub(inbound_fee_msat.unsigned_abs())
                    .max(prev_amt)
            };
            amount_msat = Amount::from_msat(next_amt);
            delay = delay.saturating_add(hop.channel_state.delay);
        }
    }
    if sendpay_route
        .first()
        .and_then(|h| h.delay)
        .is_some_and(|d| d > MAX_ROUTE_CLTV_DELTA)
    {
        return vec![];
    }
    sendpay_route
}

fn construct_first_node(
    task: &mut Task,
    config: &Config,
    lngraph: &LnGraph,
    sat_direction: SatDirection,
    destination: PubKeyBytes,
) -> Result<DijkstraNode, Error> {
    let base_delay = std::cmp::max(config.cltv_delta, config.cltv_final);
    match sat_direction {
        SatDirection::Pull => {
            let Ok((scid_dir, slingchan_inc)) =
                lngraph.get_state_no_direction(&task.other_pubkey, task.get_chan_id())
            else {
                log::warn!("{task}: channel not found in graph!");
                task.set_state(JobMessage::ChanNotInGraph);
                return Err(anyhow!("channel not found in graph"));
            };
            Ok(DijkstraNode {
                score: 0,
                channel_state: *slingchan_inc,
                destination,
                short_channel_id_dir: scid_dir,
                hops: 0,
                delay: base_delay.saturating_add(slingchan_inc.delay),
            })
        }
        SatDirection::Push => {
            let Ok((scid_dir, slingchan_out)) =
                lngraph.get_state_no_direction(&config.pubkey_bytes, task.get_chan_id())
            else {
                log::warn!("{task}: channel not found in graph!");
                task.set_state(JobMessage::ChanNotInGraph);
                return Err(anyhow!("channel not found in graph"));
            };
            Ok(DijkstraNode {
                score: 0,
                channel_state: *slingchan_out,
                destination,
                short_channel_id_dir: scid_dir,
                hops: 0,
                delay: base_delay,
            })
        }
    }
}

/// `MinScored<K, T>` holds a score `K` and a scored object `T` in
/// a pair for use with a `BinaryHeap`.
///
/// `MinScored` compares in reverse order by the score, so that we can
/// use `BinaryHeap` as a min-heap to extract the score-value pair with the
/// least score.
///
/// **Note:** `MinScored` implements a total order (`Ord`), so that it is
/// possible to use float types as scores.
#[derive(Copy, Clone, Debug)]
pub struct MinScored<K, T>(pub K, pub T);

impl<K: PartialOrd, T> PartialEq for MinScored<K, T> {
    #[inline]
    fn eq(&self, other: &MinScored<K, T>) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<K: PartialOrd, T> Eq for MinScored<K, T> {}

impl<K: PartialOrd, T> PartialOrd for MinScored<K, T> {
    #[inline]
    fn partial_cmp(&self, other: &MinScored<K, T>) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: PartialOrd, T> Ord for MinScored<K, T> {
    #[inline]
    fn cmp(&self, other: &MinScored<K, T>) -> Ordering {
        let a = &self.0;
        let b = &other.0;
        if a == b {
            Ordering::Equal
        } else if a < b {
            Ordering::Greater
        } else if a > b {
            Ordering::Less
        } else if a.ne(a) && b.ne(b) {
            // these are the NaN cases
            Ordering::Equal
        } else if a.ne(a) {
            // Order NaN less, so that it is last in the MinScore order
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}
