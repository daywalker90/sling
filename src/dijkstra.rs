use crate::model::{Config, DijkstraNode, JobMessage, Liquidity, LnGraph, Task};
use crate::util::{edge_cost, fee_total_msat_precise};
use anyhow::{anyhow, Error};
use cln_rpc::model::requests::SendpayRoute;
use cln_rpc::primitives::*;
use sling::{Job, SatDirection};
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::BinaryHeap;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};
pub fn dijkstra(
    config: &Config,
    lngraph: &LnGraph,
    job: &Job,
    task: &mut Task,
    tempbans: &HashMap<ShortChannelId, u64>,
    parallel_bans: &HashSet<ShortChannelIdDir>,
    liquidity: &HashMap<ShortChannelIdDir, Liquidity>,
) -> Result<Vec<SendpayRoute>, Error> {
    let two_weeks_ago = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 60 * 60 * 24 * 14) as u32;

    let mut visited = HashSet::with_capacity(lngraph.node_count());
    let mut scores = HashMap::new();
    let mut predecessor = HashMap::new();
    let mut visit_next = BinaryHeap::new();
    let zero_score = u64::default();

    let mut excepts = Vec::new();
    excepts.extend(parallel_bans);
    for scid in tempbans.keys() {
        excepts.push(ShortChannelIdDir {
            short_channel_id: *scid,
            direction: 0,
        });
        excepts.push(ShortChannelIdDir {
            short_channel_id: *scid,
            direction: 1,
        });
    }
    match job.sat_direction {
        SatDirection::Pull => {
            for scid in config.exclude_chans_pull.iter() {
                excepts.push(ShortChannelIdDir {
                    short_channel_id: *scid,
                    direction: 0,
                });
                excepts.push(ShortChannelIdDir {
                    short_channel_id: *scid,
                    direction: 1,
                });
            }
        }
        SatDirection::Push => {
            for scid in config.exclude_chans_push.iter() {
                excepts.push(ShortChannelIdDir {
                    short_channel_id: *scid,
                    direction: 0,
                });
                excepts.push(ShortChannelIdDir {
                    short_channel_id: *scid,
                    direction: 1,
                });
            }
        }
    }

    let (start, goal) = match job.sat_direction {
        SatDirection::Pull => (task.my_pubkey, task.other_pubkey),
        SatDirection::Push => (task.other_pubkey, task.my_pubkey),
    };
    let slingchan = construct_first_node(task, lngraph, job.sat_direction)?;
    scores.insert(start, slingchan);
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
        if goal == node {
            // debug!(
            //     "{}: arrived at goal: {}  {}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &goal,
            //     &node
            // );
            break;
        }
        let current_hops = scores.get(&node).unwrap().hops;
        if current_hops + 2 > job.get_maxhops(config.maxhops) {
            continue;
        }
        for (scid, edge) in
            lngraph.edges(&node, two_weeks_ago, task, config, job, &excepts, liquidity)
        {
            let next = edge.destination;
            if visited.contains(&next) {
                // debug!(
                //     "{}: already visited: {}",
                //     slingchan.channel.short_channel_id.to_string(),
                //     &next
                // );
                continue;
            }
            let next_score = if edge.source == task.my_pubkey {
                0
            } else {
                node_score + edge_cost(edge, job.amount_msat)
            };
            // debug!(
            //     "{}: next: {} node_score:{} next_score:{}",
            //     slingchan.channel.short_channel_id.to_string(),
            //     &next,
            //     &node_score,
            //     &next_score
            // );

            match scores.entry(next) {
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
                            destination: next,
                            hops: current_hops + 1,
                            short_channel_id: scid.short_channel_id,
                        };
                        *ent.into_mut() = dijkstra_node;
                        visit_next.push(MinScored(next_score, next));
                        predecessor.insert(next, node);
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
                        destination: next,
                        hops: current_hops + 1,
                        short_channel_id: scid.short_channel_id,
                    };
                    ent.insert(dijkstra_node);
                    visit_next.push(MinScored(next_score, next));
                    predecessor.insert(next, node);
                }
            }
        }
        visited.insert(node);
    }

    build_route(
        &predecessor,
        &goal,
        &scores,
        job,
        &start,
        &slingchan,
        config,
    )
}

fn build_route(
    predecessor: &HashMap<PublicKey, PublicKey>,
    goal: &PublicKey,
    scores: &HashMap<PublicKey, DijkstraNode>,
    job: &Job,
    start: &PublicKey,
    slingchan: &DijkstraNode,
    config: &Config,
) -> Result<Vec<SendpayRoute>, Error> {
    let mut dijkstra_path = Vec::new();
    // debug!("predecssors: {:?}", predecessor);
    let mut prev;
    match predecessor.get(goal) {
        Some(node) => prev = node,
        None => return Ok(vec![]),
    };
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
    let mut delay = config.cltv_delta;

    let first_hop = if !dijkstra_path.is_empty() {
        dijkstra_path.first().unwrap()
    } else {
        return Ok(sendpay_route);
    };

    for hop in &dijkstra_path {
        if hop == first_hop {
            let routing_scid = if let Some(rscid) = first_hop.channel_state.scid_alias {
                rscid
            } else {
                first_hop.short_channel_id
            };
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat: Amount::from_msat(job.amount_msat),
                    id: dijkstra_path.first().unwrap().destination,
                    delay,
                    channel: routing_scid,
                },
            );
        } else {
            let routing_scid = if let Some(rscid) = hop.channel_state.scid_alias {
                rscid
            } else {
                hop.short_channel_id
            };
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat,
                    id: hop.destination,
                    delay,
                    channel: routing_scid,
                },
            );
        }
        prev_amount_msat = sendpay_route.first().unwrap().amount_msat;
        amount_msat = Amount::from_msat(
            Amount::msat(&prev_amount_msat)
                + fee_total_msat_precise(
                    hop.channel_state.fee_per_millionth,
                    hop.channel_state.base_fee_millisatoshi,
                    Amount::msat(&prev_amount_msat),
                )
                .ceil() as u64,
        );
        delay += hop.channel_state.delay;
    }
    Ok(sendpay_route)
}

fn construct_first_node(
    task: &mut Task,
    lngraph: &LnGraph,
    sat_direction: SatDirection,
) -> Result<DijkstraNode, Error> {
    match sat_direction {
        SatDirection::Pull => {
            let (_scid_dir, slingchan_inc) =
                match lngraph.get_state_no_direction(&task.other_pubkey, &task.get_chan_id()) {
                    Ok(in_chan) => in_chan,
                    Err(_) => {
                        log::warn!("{}: channel not found in graph!", task);
                        task.set_state(JobMessage::ChanNotInGraph);
                        return Err(anyhow!("channel not found in graph"));
                    }
                };
            Ok(DijkstraNode {
                score: 0,
                channel_state: *slingchan_inc,
                destination: task.my_pubkey,
                short_channel_id: task.get_chan_id(),
                hops: 0,
            })
        }
        SatDirection::Push => {
            let (_scid_dir, slingchan_out) =
                match lngraph.get_state_no_direction(&task.my_pubkey, &task.get_chan_id()) {
                    Ok(in_chan) => in_chan,
                    Err(_) => {
                        log::warn!("{}: channel not found in graph!", task);
                        task.set_state(JobMessage::ChanNotInGraph);
                        return Err(anyhow!("channel not found in graph"));
                    }
                };
            Ok(DijkstraNode {
                score: 0,
                channel_state: *slingchan_out,
                destination: task.other_pubkey,
                short_channel_id: task.get_chan_id(),
                hops: 0,
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
