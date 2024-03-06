use std::cmp::max;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, path::Path, str::FromStr};

use anyhow::{anyhow, Error};
use chrono::Local;
use chrono::TimeZone;
use cln_plugin::Plugin;

use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::{PublicKey, ShortChannelId};
use log::{debug, info};
use num_format::{Locale, ToFormattedString};
use serde_json::json;
use sling::{
    ChannelPartnerStats, FailureReasonCount, FailuresInTimeWindow, PeerPartnerStats, SlingStats,
    SuccessesInTimeWindow,
};
use tabled::Table;

use crate::model::{FailureReb, SuccessReb};
use crate::model::{JobState, PluginState, StatSummary, NO_ALIAS_SET, PLUGIN_NAME};
use crate::util::{get_all_normal_channels_from_listpeerchannels, refresh_joblists};

pub async fn slingstats(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);

    let input_array = match args {
        serde_json::Value::Array(a) => a,
        _ => return Err(anyhow!("invalid arguments")),
    };
    if input_array.len() > 1 {
        return Err(anyhow!(
            "Please provide exactly one short_channel_id or nothing for a summary"
        ));
    }

    let stats_delete_successes_age = plugin
        .state()
        .config
        .lock()
        .stats_delete_successes_age
        .value;
    let stats_delete_failures_age = plugin.state().config.lock().stats_delete_failures_age.value;
    let peer_channels = plugin.state().peer_channels.lock().await.clone();

    if input_array.is_empty() {
        let mut successes = HashMap::new();
        let mut failures = HashMap::new();
        refresh_joblists(plugin.clone()).await?;
        let pull_jobs = plugin.state().pull_jobs.lock().clone();
        let push_jobs = plugin.state().push_jobs.lock().clone();
        let mut all_jobs: Vec<ShortChannelId> =
            pull_jobs.into_iter().chain(push_jobs.into_iter()).collect();

        let scid_peer_map = get_all_normal_channels_from_listpeerchannels(&peer_channels);

        let mut normal_channels_alias: HashMap<ShortChannelId, String> = HashMap::new();
        {
            let alias_map = plugin.state().alias_peer_map.lock();
            for (scid, peer) in &scid_peer_map {
                normal_channels_alias.insert(
                    *scid,
                    alias_map
                        .get(peer)
                        .unwrap_or(&"ALIAS_NOT_FOUND".to_string())
                        .clone(),
                );
            }
        }
        all_jobs.retain(|c| normal_channels_alias.contains_key(c));
        for scid in &all_jobs {
            match SuccessReb::read_from_file(&sling_dir, scid).await {
                Ok(o) => {
                    successes.insert(scid, o);
                }
                Err(e) => debug!("probably no success stats yet: {:?}", e),
            };

            match FailureReb::read_from_file(&sling_dir, scid).await {
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
        let mut table = Vec::new();
        let jobstates = plugin.state().job_state.lock().clone();

        for job in &all_jobs {
            let mut total_amount_msat = 0;
            let mut most_recent_completed_at = 0;
            let mut weighted_fee_ppm = 0;
            let jobstate: Vec<String> = jobstates
                .get(job)
                .unwrap_or(&vec![JobState::missing()])
                .iter()
                .map(|jt| jt.id().to_string() + ":" + &jt.state().to_string())
                .collect();
            for success_reb in successes.get(&job).unwrap_or(&Vec::new()) {
                if stats_delete_successes_age == 0
                    || success_reb.completed_at >= now - stats_delete_successes_age * 24 * 60 * 60
                {
                    total_amount_msat += success_reb.amount_msat;
                    weighted_fee_ppm += success_reb.fee_ppm as u64 * success_reb.amount_msat;
                    most_recent_completed_at =
                        std::cmp::max(most_recent_completed_at, success_reb.completed_at);
                }
            }
            if total_amount_msat > 0 {
                weighted_fee_ppm /= total_amount_msat;
            }

            let last_route_failure = match failures.get(&job).unwrap_or(&Vec::new()).last() {
                Some(o) => o.created_at,
                None => 0,
            };
            let last_route_success = match successes.get(&job).unwrap_or(&Vec::new()).last() {
                Some(o) => o.completed_at,
                None => 0,
            };
            let last_route_taken = if std::cmp::max(last_route_success, last_route_failure) == 0 {
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
            table.push(StatSummary {
                alias: normal_channels_alias
                    .get(&job.clone())
                    .unwrap_or(&NO_ALIAS_SET.to_string())
                    .clone(),
                scid: *job,
                pubkey: *scid_peer_map.get(&job.clone()).unwrap(),
                status: jobstate.join("\n"),
                rebamount: (total_amount_msat / 1_000).to_formatted_string(&Locale::en),
                w_feeppm: weighted_fee_ppm,
                last_route_taken,
                last_success_reb,
            })
        }
        table.sort_by_key(|x| {
            x.alias
                .chars()
                .filter(|c| c.is_ascii() && !c.is_whitespace() && c != &'@')
                .collect::<String>()
                .to_ascii_lowercase()
        });
        let tabled = Table::new(table);
        Ok(json!({"format-hint":"simple","result":format!("{}", tabled,)}))
    } else {
        let scid = match input_array.first().unwrap() {
            serde_json::Value::String(i) => ShortChannelId::from_str(i)?,
            _ => return Err(anyhow!("invalid short_channel_id")),
        };
        let successes = match SuccessReb::read_from_file(&sling_dir, &scid).await {
            Ok(o) => o,
            Err(e) => {
                info!("Could not get any successes: {}", e);
                Vec::new()
            }
        };
        let failures = match FailureReb::read_from_file(&sling_dir, &scid).await {
            Ok(o) => o,
            Err(e) => {
                info!("Could not get any failures: {}", e);
                Vec::new()
            }
        };
        let alias_map = plugin.state().alias_peer_map.lock().clone();

        let sling_stats = SlingStats {
            successes_in_time_window: success_stats(
                successes,
                stats_delete_successes_age,
                &alias_map,
                &peer_channels,
            ),
            failures_in_time_window: failure_stats(
                failures,
                stats_delete_failures_age,
                &alias_map,
                &peer_channels,
            ),
        };

        Ok(json!(sling_stats))
    }
}

fn success_stats(
    successes: Vec<SuccessReb>,
    time_window: u64,
    alias_map: &HashMap<PublicKey, String>,
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
) -> Option<SuccessesInTimeWindow> {
    if successes.is_empty() {
        return None;
    }
    let mut total_amount_msat = 0;
    let mut channel_partner_counts = HashMap::new();
    let mut hop_counts = HashMap::new();
    let mut most_recent_completed_at = 0;
    let mut total_transactions = 0;
    let mut weighted_fee_ppm = 0;
    let mut fee_ppms = Vec::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    for success_reb in successes {
        if time_window == 0 || success_reb.completed_at >= now - time_window * 24 * 60 * 60 {
            total_amount_msat += success_reb.amount_msat;
            weighted_fee_ppm += success_reb.fee_ppm as u64 * success_reb.amount_msat;
            *channel_partner_counts
                .entry(success_reb.channel_partner)
                .or_insert(0) += success_reb.amount_msat / 1_000;
            *hop_counts.entry(success_reb.hops).or_insert(0) += 1;
            most_recent_completed_at =
                std::cmp::max(most_recent_completed_at, success_reb.completed_at);
            fee_ppms.push(success_reb.fee_ppm);
            total_transactions += 1_u64;
        }
    }
    if total_transactions == 0 {
        return None;
    }
    fee_ppms.sort();
    let most_common_hop_count = hop_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(hops, _)| hops);
    weighted_fee_ppm /= total_amount_msat;

    let mut channel_partners = channel_partner_counts.into_iter().collect::<Vec<_>>();

    channel_partners.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));

    let top_5_channel_partners = if channel_partners.len() >= 5 {
        &channel_partners[..5]
    } else {
        &channel_partners[..]
    };
    let feeppm_90th_percentile =
        fee_ppms[max(0, (fee_ppms.len() as f64 * 0.9).ceil() as i32 - 1) as usize];
    let time_of_last_rebalance = Local
        .timestamp_opt(most_recent_completed_at as i64, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let successes_in_time_window = SuccessesInTimeWindow {
        time_window_days: if time_window == 0 {
            "all".to_string()
        } else {
            time_window.to_string()
        },
        total_amount_sats: total_amount_msat / 1_000,
        feeppm_weighted_avg: weighted_fee_ppm,
        feeppm_min: *fee_ppms.iter().min().unwrap(),
        feeppm_max: *fee_ppms.iter().max().unwrap(),
        feeppm_median: fee_ppms[fee_ppms.len() / 2],
        feeppm_90th_percentile,
        top_5_channel_partners: top_5_channel_partners
            .iter()
            .map(|(partner, count)| ChannelPartnerStats {
                scid: *partner,
                alias: get_stats_alias(peer_channels, partner, alias_map),
                sats: *count,
            })
            .collect::<Vec<_>>(),
        most_common_hop_count,
        time_of_last_rebalance,
        total_rebalances: total_transactions,
        total_spent_sats: (weighted_fee_ppm as f64 * 0.000001 * total_amount_msat as f64) as u64
            / 1000,
    };
    Some(successes_in_time_window)
}

fn failure_stats(
    failures: Vec<FailureReb>,
    time_window: u64,
    alias_map: &HashMap<PublicKey, String>,
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
) -> Option<FailuresInTimeWindow> {
    if failures.is_empty() {
        return None;
    }
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
        if time_window == 0 || fail_reb.created_at >= now - time_window * 24 * 60 * 60 {
            total_amount_msat += fail_reb.amount_msat;
            *channel_partner_counts
                .entry(fail_reb.channel_partner)
                .or_insert(0) += fail_reb.amount_msat / 1_000;
            *hop_counts.entry(fail_reb.hops).or_insert(0) += 1;
            *reason_counts
                .entry(fail_reb.failure_reason)
                .or_insert(0_u32) += 1;
            *fail_node_counts
                .entry(fail_reb.failure_node)
                .or_insert(0_u32) += 1;
            most_recent_created_at = std::cmp::max(most_recent_created_at, fail_reb.created_at);
            total_transactions += 1_u64;
        }
    }
    if total_transactions == 0 {
        return None;
    }
    let most_common_hop_count = hop_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(hops, _)| hops);

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
    let time_of_last_attempt = Local
        .timestamp_opt(most_recent_created_at as i64, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let failures_in_time_window = FailuresInTimeWindow {
        time_window_days: if time_window == 0 {
            "all".to_string()
        } else {
            time_window.to_string()
        },
        total_amount_tried_sats: total_amount_msat / 1_000,
        top_5_failure_reasons: top_5_failure_reasons
            .iter()
            .map(|(reason, count)| FailureReasonCount {
                failure_reason: reason.clone(),
                failure_count: *count,
            })
            .collect::<Vec<_>>(),
        top_5_fail_nodes: top_5_fail_nodes
            .iter()
            .map(|(node, count)| PeerPartnerStats {
                peer_id: *node,
                alias: alias_map
                    .get(node)
                    .unwrap_or(&"NOT_FOUND".to_string())
                    .clone(),
                count: *count,
            })
            .collect::<Vec<_>>(),
        top_5_channel_partners: top_5_channel_partners
            .iter()
            .map(|(partner, count)| ChannelPartnerStats {
                scid: *partner,
                alias: get_stats_alias(peer_channels, partner, alias_map),
                sats: *count,
            })
            .collect::<Vec<_>>(),
        most_common_hop_count,
        time_of_last_attempt,
        total_rebalances_tried: total_transactions,
    };

    Some(failures_in_time_window)
}

fn get_stats_alias(
    peer_channels: &BTreeMap<ShortChannelId, ListpeerchannelsChannels>,
    partner: &ShortChannelId,
    alias_map: &HashMap<PublicKey, String>,
) -> String {
    if let Some(chan) = &peer_channels.get(partner) {
        if let Some(alias) = alias_map.get(&chan.peer_id.unwrap()) {
            alias.clone()
        } else {
            "ALIAS_NOT_FOUND".to_string()
        }
    } else {
        "PEER_NOT_FOUND".to_string()
    }
}
