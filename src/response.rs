use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::anyhow;
use cln_plugin::{Error, Plugin};
use cln_rpc::{
    ClnRpc,
    model::{
        requests::{
            AskreneinformchannelInform,
            AskreneinformchannelRequest,
            SendpayRequest,
            SendpayRoute,
            WaitsendpayRequest,
        },
        responses::{ListpeerchannelsChannels, SendpayResponse},
    },
    primitives::{Amount, PublicKey, Sha256, ShortChannelId, ShortChannelIdDir},
};
use sling::{Job, SatDirection};
use tokio::time::Instant;

use crate::{
    Config,
    FailureReb,
    PluginState,
    SuccessReb,
    errors::WaitsendpayErrorData,
    feeppm_effective_from_amts,
    model::{Liquidity, PayResolveInfo, TaskIdentifier},
    my_sleep,
    util::get_direction_from_nodes,
};

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub async fn waitsendpay_response(
    plugin: Plugin<PluginState>,
    config: &Config,
    payment_hash: Sha256,
    task_ident: &TaskIdentifier,
    now: Instant,
    job: &Job,
    route: &[SendpayRoute],
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<u64, Error> {
    let mut rpc = ClnRpc::new(&config.rpc_path).await?;
    match rpc
        .call_typed(&WaitsendpayRequest {
            payment_hash,
            timeout: Some(u32::from(config.timeoutpay)),
            partid: None,
            groupid: None,
        })
        .await
    {
        Ok(o) => {
            log::info!(
                "{}: Rebalance SUCCESSFULL after {}s. Sent {}sats plus {}msats fee",
                task_ident,
                now.elapsed().as_secs(),
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
                    SatDirection::Pull => route.first().unwrap().channel.unwrap(),
                    SatDirection::Push => route.last().unwrap().channel.unwrap(),
                },
                hops: u8::try_from(route.len() - 1)?,
                completed_at: o.completed_at.unwrap() as u64,
            }
            .write_to_file(task_ident.get_chan_id(), &config.sling_dir)
            .await?;
            *success_route = Some(route.to_vec());
            Ok(o.amount_msat.unwrap().msat())
        }
        Err(err) => {
            *success_route = None;
            let was_unresolved = plugin
                .state()
                .pays
                .write()
                .remove(&payment_hash.to_string())
                .is_some();
            let mut special_stop = false;
            let Some(ws_code) = err.code else {
                return Err(anyhow!(
                    "{}: No WaitsendpayErrorCode, instead: {}",
                    task_ident,
                    err.message
                ));
            };

            if ws_code == 200 {
                log::warn!(
                    "{}: Rebalance WAITSENDPAY_TIMEOUT failure after {}s: {}",
                    task_ident,
                    now.elapsed().as_secs(),
                    err.message,
                );

                if !was_unresolved {
                    log::warn!(
                        "{task_ident}: HTLC preimage was already released before timeout; \
                        counting {}msat as rebalanced to avoid over-rebalancing",
                        job.amount_msat
                    );
                    return Ok(job.amount_msat);
                }

                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                for dir_chan in timeout_intermediate_channels(route, config.pubkey)? {
                    let mut liquidity = plugin.state().liquidity.lock();
                    if let Some(liq) = liquidity.get_mut(&dir_chan) {
                        liq.liquidity_msat = 0;
                        liq.liquidity_age = now_secs;
                    } else {
                        liquidity.insert(
                            dir_chan,
                            Liquidity {
                                liquidity_msat: 0,
                                liquidity_age: now_secs,
                            },
                        );
                    }
                }
                FailureReb {
                    amount_msat: job.amount_msat,
                    failure_reason: "WAITSENDPAY_TIMEOUT".to_string(),
                    failure_node: config.pubkey,
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel.unwrap(),
                        SatDirection::Push => route.last().unwrap().channel.unwrap(),
                    },
                    hops: u8::try_from(route.len() - 1)?,
                    created_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                }
                .write_to_file(task_ident.get_chan_id(), &config.sling_dir)
                .await?;
                Ok(0)
            } else if let Some(d) = err.data {
                let ws_error = match serde_json::from_value::<WaitsendpayErrorData>(d) {
                    Ok(e) => e,
                    Err(parse_err) => {
                        log::warn!(
                            "{task_ident}: Could not parse waitsendpay error data ({parse_err}): {}",
                            err.message
                        );
                        return Ok(0);
                    }
                };

                let err_amount_msat = ws_error
                    .amount_msat
                    .map_or(job.amount_msat, |a| a.msat());
                let failcodename = ws_error
                    .failcodename
                    .clone()
                    .unwrap_or_else(|| err.message.clone());
                let erring_node = ws_error.erring_node.unwrap_or(config.pubkey);

                {
                    let alias_map = plugin.state().alias_peer_map.lock();
                    log::info!(
                        "{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                        task_ident,
                        now.elapsed().as_secs(),
                        err.message,
                        alias_map
                            .get(&erring_node)
                            .unwrap_or(&erring_node.to_string()),
                        ws_error
                            .erring_channel
                            .map_or_else(|| "none".to_string(), |c| c.to_string()),
                    );
                }

                if failcodename == "WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS"
                    && ws_error.erring_node == Some(config.pubkey)
                {
                    log::warn!("{task_ident}: PAYMENT DETAILS ERROR:{failcodename:?} {route:?}");
                    special_stop = true;
                }

                FailureReb {
                    amount_msat: err_amount_msat,
                    failure_reason: failcodename.clone(),
                    failure_node: erring_node,
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel.unwrap(),
                        SatDirection::Push => route.last().unwrap().channel.unwrap(),
                    },
                    hops: u8::try_from(route.len() - 1)?,
                    created_at: ws_error.created_at,
                }
                .write_to_file(task_ident.get_chan_id(), &config.sling_dir)
                .await?;
                if special_stop {
                    return Err(anyhow!(
                        "{}: UNEXPECTED waitsendpay failure after {}s: {}",
                        task_ident,
                        now.elapsed().as_secs(),
                        err.message
                    ));
                }

                let Some(erring_channel) = ws_error.erring_channel else {
                    log::warn!(
                        "{task_ident}: Rebalance failure without erring_channel ({failcodename}); \
                        constricting intermediate hops"
                    );
                    let now_secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    for dir_chan in timeout_intermediate_channels(route, config.pubkey)? {
                        plugin.state().liquidity.lock().insert(
                            dir_chan,
                            Liquidity {
                                liquidity_msat: err_amount_msat / 2,
                                liquidity_age: now_secs,
                            },
                        );
                    }
                    return Ok(0);
                };

                let (resolved_erring, resolved_first, resolved_last) = {
                    let peer_channels = plugin.state().peer_channels.lock();
                    (
                        resolve_local_scid(&peer_channels, erring_channel),
                        resolve_local_scid(&peer_channels, route.first().unwrap().channel.unwrap()),
                        resolve_local_scid(&peer_channels, route.last().unwrap().channel.unwrap()),
                    )
                };

                if resolved_erring == resolved_last {
                    log::warn!(
                        "{}: Last peer has a problem or just updated their fees? {}",
                        task_ident,
                        failcodename
                    );

                    let last_hop = route
                        .len()
                        .checked_sub(2)
                        .and_then(|idx| route.get(idx))
                        .and_then(|h| h.id);
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(plugin.clone(), 3, task_ident).await;
                    } else if last_hop.is_some_and(|lh| {
                        plugin.state().bad_fwd_nodes.lock().contains_key(&lh)
                    }) {
                        log::debug!(
                            "{task_ident}: Last hop {} got temp banned because \
                            of bad forwarding",
                            last_hop.unwrap()
                        );
                    } else {
                        plugin.state().temp_chan_bans.lock().insert(
                            resolved_last,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                } else if resolved_erring == resolved_first {
                    log::warn!("{}: First peer has a problem {}", task_ident, err.message);
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(plugin.clone(), 3, task_ident).await;
                    } else {
                        plugin.state().temp_chan_bans.lock().insert(
                            resolved_first,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                } else if erring_channel == ShortChannelId::from(0) {
                    log::warn!(
                        "{task_ident}: Node-level failure at {erring_node} ({failcodename}), \
                        skipping 0x0x0 channel constraint"
                    );
                } else if let Some(erring_direction) = ws_error.erring_direction {
                    let dir_chan = ShortChannelIdDir {
                        short_channel_id: erring_channel,
                        direction: u32::from(erring_direction),
                    };
                    let constrained_msat = err_amount_msat / 2;
                    log::debug!(
                        "{}: Adjusting liquidity for {} to constrain it to {}msat",
                        task_ident,
                        dir_chan,
                        constrained_msat
                    );

                    {
                        let mut liquidity = plugin.state().liquidity.lock();
                        if let Some(liq) = liquidity.get_mut(&dir_chan) {
                            liq.liquidity_msat = constrained_msat;
                            liq.liquidity_age = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                        } else {
                            liquidity.insert(
                                dir_chan,
                                Liquidity {
                                    liquidity_msat: constrained_msat,
                                    liquidity_age: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs(),
                                },
                            );
                        }
                    }
                    if config.has_askrene_ll && failcodename == "WIRE_TEMPORARY_CHANNEL_FAILURE" {
                        for lay in &config.inform_layers {
                            log::debug!(
                                "{}: Informing layer `{}` about scid_dir:{} amt:{}msat constraint",
                                task_ident,
                                lay,
                                dir_chan,
                                constrained_msat
                            );
                            if let Err(e) = rpc
                                .call_typed(&AskreneinformchannelRequest {
                                    amount_msat: Amount::from_msat(constrained_msat),
                                    inform: AskreneinformchannelInform::CONSTRAINED,
                                    short_channel_id_dir: dir_chan,
                                    layer: lay.clone(),
                                })
                                .await
                            {
                                log::debug!(
                                    "{task_ident}: Could not inform askrene layer `{lay}` about \
                                    {dir_chan}: {e}"
                                );
                            }
                        }
                    }
                }
                Ok(0)
            } else {
                Err(anyhow!(
                    "{}: UNEXPECTED waitsendpay failure: {} after: {}",
                    task_ident,
                    err.message,
                    now.elapsed().as_millis()
                ))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn sendpay_response(
    plugin: Plugin<PluginState>,
    config: &Config,
    payment_hash: Sha256,
    pay_resolve_info: PayResolveInfo,
    task_ident: &TaskIdentifier,
    job: &Job,
    route: &[SendpayRoute],
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<Option<SendpayResponse>, Error> {
    let mut rpc = ClnRpc::new(&config.rpc_path).await?;
    let payment_secret = pay_resolve_info.payment_secret;
    let payment_hash_str = payment_hash.to_string();
    plugin
        .state()
        .pays
        .write()
        .insert(payment_hash_str.clone(), pay_resolve_info);
    match rpc
        .call_typed(&SendpayRequest {
            route: route.to_vec(),
            payment_hash,
            label: None,
            amount_msat: None,
            bolt11: None,
            payment_secret: Some(payment_secret),
            partid: None,
            localinvreqid: None,
            groupid: None,
            description: None,
            payment_metadata: None,
        })
        .await
    {
        Ok(resp) => Ok(Some(resp)),
        Err(e) => {
            plugin.state().pays.write().remove(&payment_hash_str);
            if e.to_string().contains("First peer not ready") {
                log::info!("{task_ident}: First peer not ready, banning it for now...");
                let first_scid = {
                    let peer_channels = plugin.state().peer_channels.lock();
                    resolve_local_scid(&peer_channels, route.first().unwrap().channel.unwrap())
                };
                plugin.state().temp_chan_bans.lock().insert(
                    first_scid,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );
                *success_route = None;
                FailureReb {
                    amount_msat: job.amount_msat,
                    failure_reason: "FIRST_PEER_NOT_READY".to_string(),
                    failure_node: route.first().unwrap().id.unwrap(),
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel.unwrap(),
                        SatDirection::Push => route.last().unwrap().channel.unwrap(),
                    },
                    hops: u8::try_from(route.len() - 1)?,
                    created_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                }
                .write_to_file(task_ident.get_chan_id(), &config.sling_dir)
                .await?;
                return Ok(None);
            }

            Err(anyhow!("{task_ident}: Unexpected sendpay error: {e}"))
        }
    }
}

pub(crate) fn timeout_intermediate_channels(
    route: &[SendpayRoute],
    our_pubkey: PublicKey,
) -> Result<Vec<ShortChannelIdDir>, Error> {
    let mut chans = Vec::new();
    for pair in route.windows(2) {
        let Some(source) = pair[0].id else {
            continue;
        };
        let Some(destination) = pair[1].id else {
            continue;
        };
        if source == our_pubkey || destination == our_pubkey {
            continue;
        }
        let Some(scid) = pair[1].channel else {
            continue;
        };
        let direction = get_direction_from_nodes(source, destination)?;
        chans.push(ShortChannelIdDir {
            short_channel_id: scid,
            direction,
        });
    }
    Ok(chans)
}

fn resolve_local_scid(
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    scid_or_alias: ShortChannelId,
) -> ShortChannelId {
    if peer_channels.contains_key(&scid_or_alias) {
        return scid_or_alias;
    }
    for chan in peer_channels.values() {
        if let Some(alias) = &chan.alias {
            if alias.local == Some(scid_or_alias) || alias.remote == Some(scid_or_alias) {
                if let Some(real_scid) = chan.short_channel_id {
                    return real_scid;
                }
            }
        }
    }
    scid_or_alias
}
