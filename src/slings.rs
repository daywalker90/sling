use anyhow::{anyhow, Error};

use cln_plugin::Plugin;

use cln_rpc::model::requests::SendpayRoute;
use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::*;

use sling::{Job, SatDirection};
use std::cmp::{max, min};

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::{self, Instant};

use crate::dijkstra::dijkstra;
use crate::model::{Config, JobMessage, PluginState, PubKeyBytes, TaskIdentifier};
use crate::response::{sendpay_response, waitsendpay_response};
use crate::util::{
    feeppm_effective, feeppm_effective_from_amts, get_normal_channel_from_listpeerchannels,
    get_preimage_paymend_hash_pair, get_total_htlc_count, is_channel_normal, my_sleep,
};
use crate::{get_remote_feeppm_effective, wait_for_gossip};

pub async fn sling(
    job: &Job,
    task_ident: TaskIdentifier,
    plugin: Plugin<PluginState>,
) -> Result<u64, Error> {
    wait_for_gossip(plugin.clone(), &task_ident).await?;

    let mut success_route: Option<Vec<SendpayRoute>> = None;
    let mut rebalanced_msat = 0;
    'outer: loop {
        let now = Instant::now();
        let task;
        {
            let tasks = plugin.state().tasks.lock();
            task = tasks
                .get_task(&task_ident)
                .ok_or_else(|| anyhow!("no task found"))?
                .clone();
        }
        if task.should_stop() {
            log::info!("{}: Stopped job!", task_ident);
            let mut tasks = plugin.state().tasks.lock();
            tasks.set_state(&task_ident, JobMessage::Stopped);
            tasks.set_active(&task_ident, false);
            break;
        }

        let config = plugin.state().config.lock().clone();

        let tempbans = plugin.state().tempbans.lock().clone();
        let peer_channels = plugin.state().peer_channels.lock().clone();

        if let Some(r) = health_check(
            plugin.clone(),
            &config,
            &peer_channels,
            &task_ident,
            job,
            task.other_pubkey,
            &tempbans,
        )
        .await?
        {
            if r {
                continue 'outer;
            }
            break 'outer;
        }

        plugin
            .state()
            .tasks
            .lock()
            .set_state(&task_ident, JobMessage::Rebalancing);

        let route = {
            let nr = next_route(
                plugin.clone(),
                &config,
                &peer_channels,
                job,
                &tempbans,
                &task_ident,
                &mut success_route,
            )
            .await;
            if nr.is_err() || nr.as_ref().unwrap().is_empty() {
                log::info!("{}: could not find a route. Sleeping...", task_ident);
                plugin
                    .state()
                    .tasks
                    .lock()
                    .set_state(&task_ident, JobMessage::NoRoute);
                if job.once_amount_msat.is_some() {
                    time::sleep(Duration::from_secs(1)).await;
                    break 'outer;
                }
                success_route = None;
                my_sleep(plugin.clone(), 600, &task_ident).await;
                continue 'outer;
            }
            nr.unwrap()
        };

        let fee_ppm_effective = feeppm_effective_from_amts(
            Amount::msat(&route.first().unwrap().amount_msat),
            Amount::msat(&route.last().unwrap().amount_msat),
        );
        log::info!(
            "{}: Found {}ppm route with {} hops. Total: {}ms",
            task_ident,
            fee_ppm_effective,
            route.len() - 1,
            now.elapsed().as_millis()
        );

        if fee_ppm_effective > job.maxppm {
            log::info!("{}: route not cheap enough! Sleeping...", task_ident);
            plugin
                .state()
                .tasks
                .lock()
                .set_state(&task_ident, JobMessage::TooExp);
            my_sleep(plugin.clone(), 600, &task_ident).await;
            success_route = None;
            continue 'outer;
        }

        {
            let alias_map = plugin.state().alias_peer_map.lock();
            for r in &route {
                log::debug!(
                    "{}: route: {} {:4} {:17} {}",
                    task_ident,
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
            plugin.clone(),
            &config,
            payment_hash,
            preimage,
            &task_ident,
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
                let mut tasks = plugin.state().tasks.lock();
                tasks.set_state(&task_ident, JobMessage::Error);
                tasks.set_active(&task_ident, false);
                log::warn!("{}", e);
                break 'outer;
            }
        };
        log::info!(
            "{}: Sent on route. Total: {}ms",
            task_ident,
            now.elapsed().as_millis()
        );

        match waitsendpay_response(
            plugin.clone(),
            &config,
            send_response.payment_hash,
            &task_ident,
            now,
            job,
            &route,
            &mut success_route,
        )
        .await
        {
            Ok(o) => {
                rebalanced_msat += o;
                if job.once_amount_msat.is_some() {
                    break 'outer;
                }
                time::sleep(Duration::from_secs(config.refresh_peers_interval)).await;
            }
            Err(e) => {
                let mut tasks = plugin.state().tasks.lock();
                tasks.set_state(&task_ident, JobMessage::Error);
                tasks.set_active(&task_ident, false);
                log::warn!("{}", e);
                break 'outer;
            }
        };
    }
    plugin
        .state()
        .tasks
        .lock()
        .get_task_mut(&task_ident)
        .ok_or_else(|| anyhow!("no task found"))?
        .parallel_ban = None;

    Ok(rebalanced_msat)
}

async fn next_route(
    plugin: Plugin<PluginState>,
    config: &Config,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    job: &Job,
    tempbans: &HashMap<ShortChannelId, u64>,
    task_ident: &TaskIdentifier,
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<Vec<SendpayRoute>, Error> {
    let graph = plugin.state().graph.lock();
    let actual_candidates = build_candidatelist(
        plugin.clone(),
        peer_channels,
        task_ident,
        job,
        tempbans,
        config,
    )?;

    let mut tasks = plugin.state().tasks.lock();
    let mut task_bans = tasks.get_parallelbans(task_ident.get_chan_id())?;
    let task = tasks
        .get_task_mut(task_ident)
        .ok_or_else(|| anyhow!("no task found"))?;

    log::debug!(
        "{}: Candidates: {}",
        task_ident,
        actual_candidates
            .iter()
            .map(|y| y.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    );
    if !tempbans.is_empty() {
        log::debug!(
            "{}: Tempbans: {}",
            task_ident,
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
    if actual_candidates.is_empty() {
        log::info!(
            "{}: No candidates found. Adjust out_ppm or wait for liquidity. Sleeping...",
            task_ident,
        );
        plugin
            .state()
            .tasks
            .lock()
            .set_state(task_ident, JobMessage::NoCandidates);
        return Err(anyhow!("No candidates found"));
    }

    let mut route = Vec::new();
    if let Some(prev_route) = success_route {
        if match job.sat_direction {
            SatDirection::Pull => actual_candidates.iter().any(|c| {
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
            SatDirection::Push => actual_candidates.iter().any(|c| {
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

    if success_route.is_none() {
        if let Some(pb) = task.parallel_ban {
            task_bans.remove(&pb);
        }
        task.parallel_ban = None;

        let mut excepts = Vec::new();
        excepts.extend(task_bans);
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

        let liquidity = plugin.state().liquidity.lock();
        match job.sat_direction {
            SatDirection::Pull => {
                route = dijkstra(
                    config,
                    &graph,
                    job,
                    task,
                    &actual_candidates,
                    &excepts,
                    &liquidity,
                )?;
            }
            SatDirection::Push => {
                route = dijkstra(
                    config,
                    &graph,
                    job,
                    task,
                    &actual_candidates,
                    &excepts,
                    &liquidity,
                )?;
            }
        }
    }
    if route.len() >= 3 {
        let route_claim_chan = route[route.len() / 2].channel;
        let route_claim_peer = route[(route.len() / 2) - 1].id;
        if let Ok((dir_chan, dir_chan_state)) = graph.get_state_no_direction(
            &PubKeyBytes::from_pubkey(&route_claim_peer),
            &route_claim_chan,
        ) {
            if dir_chan_state.source != config.pubkey_bytes
                && dir_chan_state.destination != config.pubkey_bytes
            {
                task.parallel_ban = Some(dir_chan);
            } else {
                task.parallel_ban = None;
            }
        };
    } else {
        task.parallel_ban = None;
    };
    Ok(route)
}

async fn health_check(
    plugin: Plugin<PluginState>,
    config: &Config,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    task_ident: &TaskIdentifier,
    job: &Job,
    other_peer: PubKeyBytes,
    tempbans: &HashMap<ShortChannelId, u64>,
) -> Result<Option<bool>, Error> {
    let channel =
        match get_normal_channel_from_listpeerchannels(peer_channels, &task_ident.get_chan_id()) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("{}: {}. Stopping Job.", task_ident, e);
                let mut tasks = plugin.state().tasks.lock();
                tasks.set_state(task_ident, JobMessage::ChanNotNormal);
                tasks.set_active(task_ident, false);
                return Ok(Some(false));
            }
        };

    if job.is_balanced(&channel, &task_ident.get_chan_id())
        || match job.sat_direction {
            SatDirection::Pull => Amount::msat(&channel.receivable_msat.unwrap()) < job.amount_msat,
            SatDirection::Push => Amount::msat(&channel.spendable_msat.unwrap()) < job.amount_msat,
        }
    {
        log::info!("{}: already balanced. Taking a break...", task_ident);
        plugin
            .state()
            .tasks
            .lock()
            .set_state(task_ident, JobMessage::Balanced);
        my_sleep(plugin.clone(), 600, task_ident).await;
        Ok(Some(true))
    } else if get_total_htlc_count(&channel) > config.max_htlc_count {
        log::info!(
            "{}: already more than {} pending htlcs. Taking a break...",
            task_ident,
            config.max_htlc_count
        );
        plugin
            .state()
            .tasks
            .lock()
            .set_state(task_ident, JobMessage::HTLCcapped);
        my_sleep(plugin.clone(), 10, task_ident).await;
        Ok(Some(true))
    } else {
        match peer_channels
            .values()
            .find(|x| x.peer_id == other_peer.to_pubkey())
        {
            Some(p) => {
                if !p.peer_connected {
                    log::info!("{}: not connected. Taking a break...", task_ident);
                    plugin
                        .state()
                        .tasks
                        .lock()
                        .set_state(task_ident, JobMessage::Disconnected);
                    my_sleep(plugin.clone(), 60, task_ident).await;
                    Ok(Some(true))
                } else if tempbans.contains_key(&task_ident.get_chan_id()) {
                    log::info!("{}: Job peer not ready. Taking a break...", task_ident);
                    plugin
                        .state()
                        .tasks
                        .lock()
                        .set_state(task_ident, JobMessage::PeerNotReady);
                    my_sleep(plugin.clone(), 20, task_ident).await;
                    Ok(Some(true))
                } else {
                    Ok(None)
                }
            }
            None => {
                let mut tasks = plugin.state().tasks.lock();
                tasks.set_state(task_ident, JobMessage::PeerNotFound);
                tasks.set_active(task_ident, false);
                log::warn!("{}: peer not found. Stopping job.", task_ident);
                Ok(Some(false))
            }
        }
    }
}

fn build_candidatelist(
    plugin: Plugin<PluginState>,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    task_ident: &TaskIdentifier,
    job: &Job,
    tempbans: &HashMap<ShortChannelId, u64>,
    config: &Config,
) -> Result<Vec<ShortChannelId>, Error> {
    let blockheight = *plugin.state().blockheight.lock();
    let mut candidatelist = Vec::<ShortChannelId>::new();
    let custom_candidates = job.get_candidates();

    let depleteuptopercent = job.get_depleteuptopercent(config.depleteuptopercent);
    let depleteuptoamount = job.get_deplteuptoamount(config.depleteuptoamount);

    for channel in peer_channels.values() {
        let scid = if let Some(scid) = channel.short_channel_id {
            scid
        } else {
            log::trace!(
                "{}: build_candidatelist: channel with {} has no short_channel_id",
                task_ident,
                channel.peer_id
            );
            continue;
        };

        if scid == task_ident.get_chan_id() {
            continue;
        }

        if !custom_candidates.is_empty() {
            if custom_candidates.contains(&scid) {
                log::trace!(
                    "{}: build_candidatelist: found custom candidate {}",
                    task_ident,
                    scid
                );
            } else {
                continue;
            }
        };

        if let Err(e) = is_channel_normal(channel) {
            log::trace!(
                "{}: build_candidatelist: {} is not normal: {}",
                task_ident,
                scid,
                e
            );
            continue;
        }

        if !channel.peer_connected {
            log::trace!(
                "{}: build_candidatelist: {} is not connected",
                task_ident,
                scid
            );
            continue;
        }

        if scid.block() > blockheight - config.candidates_min_age {
            log::trace!(
                "{}: build_candidatelist: {} is too new: {}>{}",
                task_ident,
                scid,
                scid.block(),
                blockheight - config.candidates_min_age
            );
            continue;
        }

        let chan_in_ppm = match get_remote_feeppm_effective(channel, job.amount_msat) {
            Ok(o) => o,
            Err(e) => {
                log::trace!(
                    "{}: build_candidatelist: could not get remote feeppm for {}: {}",
                    task_ident,
                    scid,
                    e
                );
                continue;
            }
        };

        let to_us_msat = Amount::msat(&channel.to_us_msat.unwrap());
        let total_msat = Amount::msat(&channel.total_msat.unwrap());
        let chan_out_ppm = feeppm_effective(
            channel.fee_proportional_millionths.unwrap(),
            Amount::msat(&channel.fee_base_msat.unwrap()) as u32,
            job.amount_msat,
        );

        match job.sat_direction {
            SatDirection::Pull => {
                let liquidity_target = max(
                    job.amount_msat + 10_000_000,
                    min(
                        (depleteuptopercent * total_msat as f64) as u64,
                        depleteuptoamount,
                    ),
                );
                if to_us_msat <= liquidity_target {
                    log::trace!(
                        "{}: build_candidatelist: {} does not have enough liquidity: {}<={}",
                        task_ident,
                        scid,
                        to_us_msat,
                        liquidity_target
                    );
                    continue;
                }
                if let Some(outppm) = job.outppm {
                    if chan_out_ppm > outppm {
                        log::trace!(
                            "{}: build_candidatelist: {} outppm is too high: {}>{}",
                            task_ident,
                            scid,
                            chan_out_ppm,
                            outppm
                        );
                        continue;
                    }
                }
            }
            SatDirection::Push => {
                let liquidity_target = max(
                    job.amount_msat + 10_000_000,
                    min(
                        (depleteuptopercent * total_msat as f64) as u64,
                        depleteuptoamount,
                    ),
                );
                if total_msat - to_us_msat <= liquidity_target {
                    log::trace!(
                        "{}: build_candidatelist: {} does not have enough liquidity: {}<={}",
                        task_ident,
                        scid,
                        total_msat - to_us_msat,
                        liquidity_target
                    );
                    continue;
                }
                if let Some(outppm) = job.outppm {
                    if chan_out_ppm < outppm {
                        log::trace!(
                            "{}: build_candidatelist: {} outppm is too low: {}<{}",
                            task_ident,
                            scid,
                            chan_out_ppm,
                            outppm
                        );
                        continue;
                    }
                }
                if chan_in_ppm > (job.maxppm as u64) {
                    log::trace!(
                        "{}: build_candidatelist: {} inppm is too high: {}>{}",
                        task_ident,
                        scid,
                        chan_in_ppm,
                        job.maxppm
                    );
                    continue;
                }
            }
        };

        if tempbans.contains_key(&scid) {
            log::trace!(
                "{}: build_candidatelist: {} is temporarily banned",
                task_ident,
                scid
            );
            continue;
        }

        if get_total_htlc_count(channel) > config.max_htlc_count {
            log::trace!(
                "{}: build_candidatelist: {} has too many pending htlcs",
                task_ident,
                scid
            );
            continue;
        }

        candidatelist.push(scid);
    }

    Ok(candidatelist)
}
