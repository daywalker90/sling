use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, path::Path, str::FromStr};

use anyhow::{anyhow, Error};
use chrono::Local;
use chrono::TimeZone;
use cln_plugin::Plugin;

use cln_rpc::primitives::ShortChannelId;
use log::{debug, info};
use num_format::{Locale, ToFormattedString};
use serde_json::json;
use tabled::TableIteratorExt;

use crate::model::{JobState, PluginState, StatSummary};
use crate::util::{get_all_normal_channels_from_listpeers, refresh_joblists};
use crate::NO_ALIAS_SET;
use crate::{
    model::{FailureReb, SuccessReb},
    PLUGIN_NAME,
};

pub async fn slingstats(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);

    match args {
        serde_json::Value::Array(a) => {
            let stats_delete_successes_age =
                plugin.state().config.lock().stats_delete_successes_age.1;
            let stats_delete_failures_age =
                plugin.state().config.lock().stats_delete_failures_age.1;
            if a.len() > 1 {
                Err(anyhow!(
                    "Please provide exactly one short_channel_id or nothing for a summary"
                ))
            } else if a.len() == 0 {
                let mut successes = HashMap::new();
                let mut failures = HashMap::new();
                refresh_joblists(plugin.clone()).await?;
                let pull_jobs = plugin.state().pull_jobs.lock().clone();
                let push_jobs = plugin.state().push_jobs.lock().clone();
                let mut all_jobs: Vec<String> =
                    pull_jobs.into_iter().chain(push_jobs.into_iter()).collect();
                let alias_map = plugin.state().alias_peer_map.lock().clone();
                let peers = plugin.state().peers.lock().clone();
                let mut normal_channels_alias: HashMap<String, String> = HashMap::new();
                let scid_peer_map = get_all_normal_channels_from_listpeers(&peers);
                for (scid, peer) in &scid_peer_map {
                    normal_channels_alias.insert(
                        scid.clone(),
                        alias_map
                            .get(&peer)
                            .unwrap_or(&"ALIAS_NOT_FOUND".to_string())
                            .clone(),
                    );
                }
                all_jobs.retain(|c| normal_channels_alias.contains_key(c));
                for scid in &all_jobs {
                    match SuccessReb::read_from_file(
                        &sling_dir,
                        ShortChannelId::from_str(&scid.clone())?,
                    )
                    .await
                    {
                        Ok(o) => {
                            successes.insert(scid, o);
                        }
                        Err(e) => debug!("probably no success stats yet: {:?}", e),
                    };

                    match FailureReb::read_from_file(
                        &sling_dir,
                        ShortChannelId::from_str(&scid.clone())?,
                    )
                    .await
                    {
                        Ok(o) => {
                            failures.insert(scid, o);
                        }
                        Err(e) => debug!("probably no failure stats yet: {:?}", e),
                    };
                }

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let mut tabled = Vec::new();
                let jobstates = plugin.state().job_state.lock().clone();

                for job in &all_jobs {
                    let mut total_amount_msat = 0;
                    let mut most_recent_completed_at = 0;
                    let mut weighted_fee_ppm = 0;
                    let jobstate = jobstates.get(job).unwrap_or(JobState::missing());
                    for success_reb in successes.get(&job).unwrap_or(&Vec::new()) {
                        if success_reb.completed_at
                            >= now - stats_delete_successes_age * 24 * 60 * 60
                        {
                            total_amount_msat += success_reb.amount_msat;
                            weighted_fee_ppm +=
                                success_reb.fee_ppm as u64 * success_reb.amount_msat;
                            most_recent_completed_at =
                                std::cmp::max(most_recent_completed_at, success_reb.completed_at);
                        }
                    }
                    if total_amount_msat > 0 {
                        weighted_fee_ppm = weighted_fee_ppm / total_amount_msat;
                    }

                    let last_route_failure = match failures.get(&job).unwrap_or(&Vec::new()).last()
                    {
                        Some(o) => o.created_at,
                        None => 0,
                    };
                    let last_route_success = match successes.get(&job).unwrap_or(&Vec::new()).last()
                    {
                        Some(o) => o.completed_at,
                        None => 0,
                    };
                    let last_route_taken =
                        if std::cmp::max(last_route_success, last_route_failure) == 0 {
                            "Never".to_string()
                        } else {
                            Local
                                .timestamp_opt(
                                    std::cmp::max(last_route_success, last_route_failure) as i64,
                                    0,
                                )
                                .unwrap()
                                .format("%Y-%m-%d %H:%M:%S")
                                .to_string()
                        };
                    let last_success_reb = if last_route_success == 0 {
                        "Never".to_string()
                    } else {
                        Local
                            .timestamp_opt(last_route_success as i64, 0)
                            .unwrap()
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    };
                    tabled.push(StatSummary {
                        alias: normal_channels_alias
                            .get(&job.clone())
                            .unwrap_or(&NO_ALIAS_SET.to_string())
                            .clone(),
                        scid: job.clone(),
                        pubkey: scid_peer_map.get(&job.clone()).unwrap().to_string(),
                        status: jobstate.state().to_string(),
                        rebamount: (total_amount_msat / 1_000).to_formatted_string(&Locale::en),
                        w_feeppm: weighted_fee_ppm,
                        last_route_taken,
                        last_success_reb,
                    })
                }
                tabled.sort_by_key(|k| k.alias.clone());
                Ok(
                    json!({"format-hint":"simple","result":format!("{}", tabled.table().to_string(),)}),
                )
            } else {
                match a.first().unwrap() {
                    serde_json::Value::String(i) => {
                        let scid = ShortChannelId::from_str(&i)?;
                        let successes = match SuccessReb::read_from_file(&sling_dir, scid).await {
                            Ok(o) => o,
                            Err(e) => {
                                info!("Could not get any successes: {}", e);
                                Vec::new()
                            }
                        };
                        let failures = match FailureReb::read_from_file(&sling_dir, scid).await {
                            Ok(o) => o,
                            Err(e) => {
                                info!("Could not get any failures: {}", e);
                                Vec::new()
                            }
                        };
                        let success_json = if successes.len() > 0 {
                            success_stats(successes, stats_delete_successes_age)
                        } else {
                            json!({"successes_in_time_window":""})
                        };
                        let failures_json = if failures.len() > 0 {
                            failure_stats(failures, stats_delete_failures_age)
                        } else {
                            json!({"failures_in_time_window":""})
                        };

                        Ok(
                            json!({ "successes_in_time_window":success_json.get("successes_in_time_window").unwrap(),
                        "failures_in_time_window":failures_json.get("failures_in_time_window") }),
                        )
                    }
                    _ => Err(anyhow!("invalid short_channel_id")),
                }
            }
        }
        _ => Err(anyhow!("invalid arguments")),
    }
}

fn success_stats(successes: Vec<SuccessReb>, time_window: u64) -> serde_json::Value {
    let mut total_amount_msat = 0;
    let mut channel_partner_counts = HashMap::new();
    let mut hop_counts = HashMap::new();
    let mut most_recent_completed_at = 0;
    let mut total_transactions = 0;
    let mut weighted_fee_ppm = 0;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    for success_reb in successes {
        if success_reb.completed_at >= now - time_window * 24 * 60 * 60 {
            total_amount_msat += success_reb.amount_msat;
            weighted_fee_ppm += success_reb.fee_ppm as u64 * success_reb.amount_msat;
            *channel_partner_counts
                .entry(success_reb.channel_partner.to_string())
                .or_insert(0) += success_reb.amount_msat / 1_000;
            *hop_counts.entry(success_reb.hops).or_insert(0) += 1;
            most_recent_completed_at =
                std::cmp::max(most_recent_completed_at, success_reb.completed_at);
            total_transactions += 1;
        }
    }
    if total_transactions == 0 {
        return json!({"successes_in_time_window":""});
    }
    let most_common_hop_count = hop_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(hops, _)| hops);
    let average_amount_msat = total_amount_msat as f64 / total_transactions as f64;
    weighted_fee_ppm = weighted_fee_ppm / total_amount_msat;

    let mut channel_partners = channel_partner_counts.into_iter().collect::<Vec<_>>();

    channel_partners.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));

    let top_5_channel_partners = if channel_partners.len() >= 5 {
        &channel_partners[..5]
    } else {
        &channel_partners[..]
    };

    json!({"successes_in_time_window":{
        "total_amount_sats":total_amount_msat/1_000,
        "average_amount_sats":(average_amount_msat/1_000.0) as u64,
        "weighted_fee_ppm":weighted_fee_ppm,
        "top_5_channel_partners":top_5_channel_partners.iter().map(|(partner, count)| {
            json!({partner: count})
        }).collect::<Vec<_>>(),
        "most_common_hop_count":most_common_hop_count,
        "most_recent_completed_at": Local.timestamp_opt(most_recent_completed_at as i64,0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string(),
        "total_rebalances":total_transactions
    }})
}

fn failure_stats(failures: Vec<FailureReb>, time_window: u64) -> serde_json::Value {
    let mut total_amount_msat = 0;
    let mut channel_partner_counts = HashMap::new();
    let mut hop_counts = HashMap::new();
    let mut reason_counts = HashMap::new();
    let mut fail_node_counts = HashMap::new();
    let mut most_recent_created_at = 0;
    let mut total_transactions = 0;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    for fail_reb in failures {
        if fail_reb.created_at >= now - time_window * 24 * 60 * 60 {
            total_amount_msat += fail_reb.amount_msat;
            *channel_partner_counts
                .entry(fail_reb.channel_partner.to_string())
                .or_insert(0) += fail_reb.amount_msat / 1_000;
            *hop_counts.entry(fail_reb.hops).or_insert(0) += 1;
            *reason_counts.entry(fail_reb.failure_reason).or_insert(0) += 1;
            *fail_node_counts
                .entry(fail_reb.failure_node.to_string())
                .or_insert(0) += 1;
            most_recent_created_at = std::cmp::max(most_recent_created_at, fail_reb.created_at);
            total_transactions += 1;
        }
    }
    if total_transactions == 0 {
        return json!({"failures_in_time_window":""});
    }
    let most_common_hop_count = hop_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(hops, _)| hops);
    let average_amount_msat = total_amount_msat as f64 / total_transactions as f64;

    let mut channel_partners = channel_partner_counts.into_iter().collect::<Vec<_>>();
    let mut failure_reasons = reason_counts.into_iter().collect::<Vec<_>>();
    let mut fail_nodes = fail_node_counts.into_iter().collect::<Vec<_>>();

    channel_partners.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));
    failure_reasons.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));
    fail_nodes.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));

    let top_5_channel_partners = if channel_partners.len() >= 5 {
        &channel_partners[..5]
    } else {
        &channel_partners[..]
    };
    let top_5_failure_reasons = if failure_reasons.len() >= 5 {
        &failure_reasons[..5]
    } else {
        &failure_reasons[..]
    };
    let top_5_fail_nodes = if fail_nodes.len() >= 5 {
        &fail_nodes[..5]
    } else {
        &fail_nodes[..]
    };

    json!({"failures_in_time_window":{
        "total_amount_tried_sats":total_amount_msat/1_000,
        "average_amount_tried_sats":(average_amount_msat/1_000.0) as u64,
        "top_5_failure_reasons":top_5_failure_reasons.iter().map(|(reason, count)| {
            json!({reason: count})
        }).collect::<Vec<_>>(),
        "top_5_fail_nodes":top_5_fail_nodes.iter().map(|(node, count)| {
            json!({node: count})
        }).collect::<Vec<_>>(),
        "top_5_channel_partners":top_5_channel_partners.iter().map(|(partner, count)| {
            json!({partner: count})
        }).collect::<Vec<_>>(),
        "most_common_hop_count":most_common_hop_count,
        "most_recent_created_at": Local.timestamp_opt(most_recent_created_at as i64,0).unwrap().format("%Y-%m-%d %H:%M:%S").to_string(),
        "total_rebalances_tried":total_transactions
    }})
}
