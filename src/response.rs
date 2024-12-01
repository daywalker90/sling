use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::anyhow;
use cln_plugin::{Error, Plugin};
use cln_rpc::{
    model::{
        requests::{
            AskreneinformchannelInform, AskreneinformchannelRequest, SendpayRequest, SendpayRoute,
            WaitsendpayRequest,
        },
        responses::SendpayResponse,
    },
    primitives::{Amount, Sha256, ShortChannelId, ShortChannelIdDir},
    ClnRpc,
};
use log::{debug, info, warn};
use sling::{Job, SatDirection};
use tokio::time::Instant;

use crate::{
    at_or_above_version, errors::WaitsendpayErrorData, feeppm_effective_from_amts, my_sleep,
    Config, FailureReb, PluginState, SuccessReb, Task,
};

#[allow(clippy::too_many_arguments)]
pub async fn waitsendpay_response(
    plugin: &Plugin<PluginState>,
    config: &Config,
    payment_hash: Sha256,
    task: &Task,
    now: Instant,
    job: &Job,
    route: &[SendpayRoute],
    success_route: &mut Option<Vec<SendpayRoute>>,
) -> Result<Option<ShortChannelId>, Error> {
    let mut rpc = ClnRpc::new(&config.rpc_path).await?;
    match rpc
        .call_typed(&WaitsendpayRequest {
            payment_hash,
            timeout: Some(config.timeoutpay.value as u32),
            partid: None,
            groupid: None,
        })
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
            .write_to_file(task.chan_id, &config.sling_dir)
            .await?;
            *success_route = Some(route.to_vec());
            Ok(None)
        }
        Err(err) => {
            *success_route = None;
            plugin
                .state()
                .pays
                .write()
                .remove(&payment_hash.to_string());
            let mut special_stop = false;
            let ws_code = if let Some(c) = err.code {
                c
            } else {
                return Err(anyhow!(
                    "{}/{}: No WaitsendpayErrorCode, instead: {}",
                    task.chan_id,
                    task.task_id,
                    err.message
                ));
            };

            if ws_code == 200 {
                warn!(
                    "{}/{}: Rebalance WAITSENDPAY_TIMEOUT failure after {}s: {}",
                    task.chan_id,
                    task.task_id,
                    now.elapsed().as_secs().to_string(),
                    err.message,
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
                            .find_map(|(dir_chan, x)| {
                                if dir_chan.short_channel_id == hop.channel
                                    && x.destination != config.pubkey
                                    && x.source != config.pubkey
                                {
                                    x.liquidity = 0;
                                    x.liquidity_age = SystemTime::now()
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
                    failure_node: config.pubkey,
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
                .write_to_file(task.chan_id, &config.sling_dir)
                .await?;
                Ok(None)
            } else if let Some(d) = err.data {
                let ws_error = serde_json::from_value::<WaitsendpayErrorData>(d)?;

                info!(
                    "{}/{}: Rebalance failure after {}s: {} at node:{} chan:{}",
                    task.chan_id,
                    task.task_id,
                    now.elapsed().as_secs().to_string(),
                    err.message,
                    ws_error.erring_node,
                    ws_error.erring_channel,
                );

                match &ws_error.failcodename {
                    err if err.eq("WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS")
                        && ws_error.erring_node == config.pubkey =>
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
                .write_to_file(task.chan_id, &config.sling_dir)
                .await?;
                if special_stop {
                    return Err(anyhow!(
                        "{}/{}: UNEXPECTED waitsendpay failure after {}s: {}",
                        task.chan_id,
                        task.task_id,
                        now.elapsed().as_secs().to_string(),
                        err.message
                    ));
                }

                if ws_error.erring_channel == route.last().unwrap().channel {
                    warn!(
                        "{}/{}: Last peer has a problem or just updated their fees? {}",
                        task.chan_id, task.task_id, ws_error.failcodename
                    );
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(3, plugin.state().job_state.clone(), task).await;
                    } else {
                        plugin.state().tempbans.lock().insert(
                            route.last().unwrap().channel,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                    }
                } else if ws_error.erring_channel == route.first().unwrap().channel {
                    warn!(
                        "{}/{}: First peer has a problem {}",
                        task.chan_id,
                        task.task_id,
                        err.message.clone()
                    );
                    if err.message.contains("Too many HTLCs") {
                        my_sleep(3, plugin.state().job_state.clone(), task).await;
                    } else {
                        plugin.state().tempbans.lock().insert(
                            route.first().unwrap().channel,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
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
                        .graph
                        .get_mut(&ws_error.erring_node)
                        .unwrap()
                        .iter_mut()
                        .find_map(|(dir_chan, x)| {
                            if dir_chan.short_channel_id == ws_error.erring_channel
                                && x.destination != config.pubkey
                                && x.source != config.pubkey
                            {
                                x.liquidity = ws_error.amount_msat.unwrap().msat() - 1;
                                x.liquidity_age = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs();
                                Some(x)
                            } else {
                                None
                            }
                        });
                    if at_or_above_version(&config.version, "24.11")? {
                        for lay in &config.inform_layers {
                            rpc.call_typed(&AskreneinformchannelRequest {
                                amount_msat: Some(ws_error.amount_msat.unwrap()),
                                inform: Some(AskreneinformchannelInform::CONSTRAINED),
                                short_channel_id_dir: Some(ShortChannelIdDir::from_str(&format!(
                                    "{}/{}",
                                    ws_error.erring_channel, ws_error.erring_direction
                                ))?),
                                layer: lay.clone(),
                            })
                            .await?;
                        }
                    }
                }
                Ok(Some(ws_error.erring_channel))
            } else {
                return Err(anyhow!(
                    "{}/{}: UNEXPECTED waitsendpay failure: {} after: {}",
                    task.chan_id,
                    task.task_id,
                    err.message,
                    now.elapsed().as_millis().to_string()
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn sendpay_response(
    plugin: &Plugin<PluginState>,
    config: &Config,
    payment_hash: Sha256,
    preimage: String,
    task: &Task,
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
                .insert(payment_hash.to_string(), preimage);
            Ok(Some(resp))
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
                *success_route = None;
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
                .write_to_file(task.chan_id, &config.sling_dir)
                .await?;
                return Ok(None);
            }

            Err(anyhow!(
                "{}/{}: Unexpected sendpay error: {}",
                task.chan_id,
                task.task_id,
                e.to_string()
            ))
        }
    }
}
