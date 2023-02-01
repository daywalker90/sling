use crate::model::{DijkstraNode, Job, LnGraph, SatDirection};
use crate::sling::{edge_cost, fee_total_msat_precise};
use anyhow::Error;
use cln_rpc::{model::*, primitives::*};
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::BinaryHeap;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

pub fn dijkstra(
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
            sendpay_route.insert(
                0,
                SendpayRoute {
                    amount_msat,
                    id: hop.destination,
                    delay,
                    channel: hop.channel.short_channel_id,
                },
            );
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
