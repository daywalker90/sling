use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use cln_plugin::{Error, Plugin};
use cln_rpc::{
    model::{
        requests::{
            AskreneinformchannelInform,
            AskreneinformchannelRequest,
            SendpayRequest,
            SendpayRoute,
            WaitsendpayRequest,
        },
        responses::SendpayResponse,
    },
    primitives::{Amount, Sha256, ShortChannelIdDir},
    ClnRpc,
};
use sling::{Job, SatDirection};
use tokio::time::Instant;

use crate::{
    errors::WaitsendpayErrorData,
    feeppm_effective_from_amts,
    model::{Liquidity, PayResolveInfo, TaskIdentifier},
    my_sleep,
    util::get_direction_from_nodes,
    Config,
    FailureReb,
    PluginState,
    SuccessReb,
};

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
                    SatDirection::Pull => route.first().unwrap().channel,
                    SatDirection::Push => route.last().unwrap().channel,
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
            plugin
                .state()
                .pays
                .write()
                .remove(&payment_hash.to_string());
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

                for (i, hop) in route[..route.len() - 1].iter().enumerate() {
                    let source = route[i].id;
                    let destination = route[i + 1].id;
                    let direction = get_direction_from_nodes(source, destination)?;
                    if source == config.pubkey {
                        continue;
                    }
                    if destination == config.pubkey {
                        continue;
                    }
                    let dir_chan = ShortChannelIdDir {
                        short_channel_id: hop.channel,
                        direction,
                    };

                    let mut liquidity = plugin.state().liquidity.lock();
                    if let Some(liq) = liquidity.get_mut(&dir_chan) {
                        liq.liquidity_msat = 0;
                        liq.liquidity_age = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                    } else {
                        liquidity.insert(
                            dir_chan,
                            Liquidity {
                                liquidity_msat: 0,
                                liquidity_age: SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs(),
                            },
                        );
                    }
                }
                FailureReb {
                    amount_msat: job.amount_msat,
                    failure_reason: "WAITSENDPAY_TIMEOUT".to_string(),
                    failure_node: config.pubkey,
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel,
                        SatDirection::Push => route.last().unwrap().channel,
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
                let ws_error = serde_json::from_value::<WaitsendpayErrorData>(d)?;

                {
                    let alias_map = plugin.state().alias_peer_map.lock();
                    log::info!(
                        "{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                        task_ident,
                        now.elapsed().as_secs(),
                        err.message,
                        alias_map
                            .get(&ws_error.erring_node)
                            .unwrap_or(&ws_error.erring_node.to_string()),
                        ws_error.erring_channel,
                    );
                }

                match &ws_error.failcodename {
                    err if err.eq("WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS")
                        && ws_error.erring_node == config.pubkey =>
                    {
                        log::warn!("{task_ident}: PAYMENT DETAILS ERROR:{err:?} {route:?}");
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

                if ws_error.erring_channel == route.last().unwrap().channel {
                    log::warn!(
                        "{}: Last peer has a problem or just updated their fees? {}",
                        task_ident,
                        ws_error.failcodename
                    );

                    let last_hop = route.get(route.len() - 2).unwrap().id;
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(plugin.clone(), 3, task_ident).await;
                    } else if plugin.state().bad_fwd_nodes.lock().contains_key(&last_hop) {
                        log::debug!(
                            "{task_ident}: Last hop {last_hop} got temp banned because \
                            of bad forwarding"
                        );
                    } else {
                        plugin.state().temp_chan_bans.lock().insert(
                            route.last().unwrap().channel,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                } else if ws_error.erring_channel == route.first().unwrap().channel {
                    log::warn!("{}: First peer has a problem {}", task_ident, err.message);
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(plugin.clone(), 3, task_ident).await;
                    } else {
                        plugin.state().temp_chan_bans.lock().insert(
                            route.first().unwrap().channel,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                } else {
                    let dir_chan = ShortChannelIdDir {
                        short_channel_id: ws_error.erring_channel,
                        direction: u32::from(ws_error.erring_direction),
                    };
                    log::debug!(
                        "{}: Adjusting liquidity for {} to constrain it to {}msat",
                        task_ident,
                        dir_chan,
                        ws_error.amount_msat.unwrap().msat() / 2
                    );

                    {
                        let mut liquidity = plugin.state().liquidity.lock();
                        if let Some(liq) = liquidity.get_mut(&dir_chan) {
                            liq.liquidity_msat = ws_error.amount_msat.unwrap().msat() / 2;
                            liq.liquidity_age = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                        } else {
                            liquidity.insert(
                                dir_chan,
                                Liquidity {
                                    liquidity_msat: ws_error.amount_msat.unwrap().msat() / 2,
                                    liquidity_age: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs(),
                                },
                            );
                        }
                    }
                    if config.at_or_above_24_11 {
                        for lay in &config.inform_layers {
                            log::debug!(
                                "{}: Informing layer `{}` about scid_dir:{} amt:{}msat constraint",
                                task_ident,
                                lay,
                                dir_chan,
                                ws_error.amount_msat.unwrap().msat() / 2
                            );
                            rpc.call_typed(&AskreneinformchannelRequest {
                                amount_msat: Some(Amount::from_msat(
                                    ws_error.amount_msat.unwrap().msat() / 2,
                                )),
                                inform: Some(AskreneinformchannelInform::CONSTRAINED),
                                short_channel_id_dir: Some(dir_chan),
                                layer: lay.clone(),
                            })
                            .await?;
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
    match rpc
        .call_typed(&SendpayRequest {
            route: route.to_vec(),
            payment_hash,
            label: None,
            amount_msat: None,
            bolt11: None,
            payment_secret: None,
            partid: None,
            localinvreqid: None,
            groupid: None,
            description: None,
            payment_metadata: None,
        })
        .await
    {
        Ok(resp) => {
            plugin
                .state()
                .pays
                .write()
                .insert(payment_hash.to_string(), pay_resolve_info);
            Ok(Some(resp))
        }
        Err(e) => {
            if e.to_string().contains("First peer not ready") {
                log::info!("{task_ident}: First peer not ready, banning it for now...");
                plugin.state().temp_chan_bans.lock().insert(
                    route.first().unwrap().channel,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );
                *success_route = None;
                FailureReb {
                    amount_msat: job.amount_msat,
                    failure_reason: "FIRST_PEER_NOT_READY".to_string(),
                    failure_node: route.first().unwrap().id,
                    channel_partner: match job.sat_direction {
                        SatDirection::Pull => route.first().unwrap().channel,
                        SatDirection::Push => route.last().unwrap().channel,
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
