use std::cmp::max;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, path::Path, str::FromStr};

use anyhow::{anyhow, Error};
use chrono::Local;
use chrono::TimeZone;
use cln_plugin::Plugin;

use cln_rpc::model::responses::ListpeerchannelsChannels;
use cln_rpc::primitives::{PublicKey, ShortChannelId};
use num_format::{Locale, ToFormattedString};
use serde_json::json;
use sling::{
    ChannelPartnerStats, FailureReasonCount, FailuresInTimeWindow, PeerPartnerStats, SlingStats,
    SuccessesInTimeWindow,
};
use tabled::settings::{Panel, Rotate};
use tabled::Table;

use crate::model::{FailureReb, JobMessage, SuccessReb};
use crate::model::{PluginState, StatSummary, NO_ALIAS_SET, PLUGIN_NAME};
use crate::util::{get_all_normal_channels_from_listpeerchannels, read_jobs};

pub async fn slingstats(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let _rpc_lock = plugin.state().rpc_lock.lock().await;

    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);

    let (scid, json_summary) = match args {
        serde_json::Value::Array(a) => {
            if a.len() > 2 {
                return Err(anyhow!(
                    "Too many arguments, please only provide `scid` and/or `json`"
                ));
            }
            if a.is_empty() {
                (None, false)
            } else if let Some(flag) = a.first().unwrap().as_bool() {
                (None, flag)
            } else {
                let scid = match a.first().unwrap() {
                    serde_json::Value::String(i) => ShortChannelId::from_str(i)?,
                    _ => return Err(anyhow!("invalid short_channel_id")),
                };
                let json_flag = match a.get(1) {
                    Some(serde_json::Value::Bool(i)) => *i,
                    None => false,
                    _ => {
                        return Err(anyhow!(
                            "invalid `json` flag, not a bool. Use `true` or `false`"
                        ))
                    }
                };
                (Some(scid), json_flag)
            }
        }
        serde_json::Value::Object(o) => {
            let scid = match o.get("scid") {
                Some(serde_json::Value::String(i)) => Some(ShortChannelId::from_str(i)?),
                None => None,
                _ => return Err(anyhow!("invalid scid, not a string")),
            };
            let json_summary = match o.get("json") {
                Some(serde_json::Value::Bool(i)) => *i,
                None => false,
                _ => {
                    return Err(anyhow!(
                        "invalid `json` flag, not a bool. Use `true` or `false`"
                    ))
                }
            };
            (scid, json_summary)
        }
        e => {
            return Err(anyhow!(
                "sling-stats: invalid arguments, expected array or object, got: {}",
                e
            ))
        }
    };

    let config = plugin.state().config.lock().clone();
    let stats_delete_successes_age = config.stats_delete_successes_age;
    let stats_delete_failures_age = config.stats_delete_failures_age;

    let peer_channels = plugin.state().peer_channels.lock().clone();

    if let Some(s) = scid {
        let successes = SuccessReb::read_from_files(&sling_dir, Some(s))
            .await
            .unwrap_or_default();
        log::debug!("successes_read: {} scid:{}", successes.len(), s);
        let failures = FailureReb::read_from_files(&sling_dir, Some(s))
            .await
            .unwrap_or_default();
        log::debug!("failures_read: {} scid:{}", successes.len(), s);
        let alias_map = plugin.state().alias_peer_map.lock().clone();

        let successes_vec = if successes.len() == 1 {
            successes.iter().last().unwrap().1.clone()
        } else {
            Vec::new()
        };
        log::debug!("successes: {} scid:{}", successes.len(), s);
        let failures_vec = if failures.len() == 1 {
            failures.iter().last().unwrap().1.clone()
        } else {
            Vec::new()
        };
        log::debug!("failures: {} scid:{}", successes.len(), s);

        let success_stats = success_stats(
            successes_vec,
            stats_delete_successes_age,
            &alias_map,
            &peer_channels,
        );
        let failure_stats = failure_stats(
            failures_vec,
            stats_delete_failures_age,
            &alias_map,
            &peer_channels,
        );

        if json_summary {
            let sling_stats = SlingStats {
                successes_in_time_window: success_stats,
                failures_in_time_window: failure_stats,
            };

            Ok(json!(sling_stats))
        } else {
            let succ_str = if let Some(ss) = success_stats {
                let mut succ_tabled = Table::new(vec![ss]);
                succ_tabled.with(Rotate::Left);
                succ_tabled.with(Panel::header("Successes in time window"));
                succ_tabled.to_string()
            } else {
                String::new()
            };
            let fail_str = if let Some(fs) = failure_stats {
                let mut fail_tabled = Table::new(vec![fs]);
                fail_tabled.with(Rotate::Left);
                fail_tabled.with(Panel::header("Failures in time window"));
                fail_tabled.to_string()
            } else {
                String::new()
            };

            Ok(json!({"format-hint":"simple","result":format!("{}\n{}", succ_str, fail_str)}))
        }
    } else {
        let successes = SuccessReb::read_from_files(&sling_dir, None).await?;
        let failures = FailureReb::read_from_files(&sling_dir, None).await?;
        let jobs = read_jobs(&sling_dir, plugin.clone()).await?;

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

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut table = Vec::new();

        let tasks = plugin.state().tasks.lock().clone();

        let mut job_scids: Vec<&ShortChannelId> = jobs.keys().collect();
        for (scid, tasks) in tasks.get_all_tasks() {
            if tasks.values().any(|t| t.is_once()) {
                job_scids.push(scid);
            }
        }

        for scid in job_scids {
            let tasks = tasks.get_scid_tasks(scid);
            let mut task_states: Vec<(u16, String)> = if let Some(task) = tasks {
                task.iter()
                    .map(|(id, jt)| (*id, id.to_string() + ":" + &jt.get_state().to_string()))
                    .collect()
            } else {
                vec![(
                    1,
                    "1".to_string() + ":" + JobMessage::NotStarted.to_string().as_str(),
                )]
            };
            task_states.sort_by(|a, b| a.0.cmp(&b.0));
            let task_states: Vec<String> =
                task_states.into_iter().map(|(_id, state)| state).collect();

            let mut total_amount_msat = 0;
            let mut most_recent_completed_at = 0;
            let mut weighted_fee_ppm = 0;
            for success_reb in successes.get(scid).unwrap_or(&Vec::new()) {
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

            let last_route_failure = match failures.get(scid).unwrap_or(&Vec::new()).last() {
                Some(o) => o.created_at,
                None => 0,
            };
            let last_route_success = match successes.get(scid).unwrap_or(&Vec::new()).last() {
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
                    .get(scid)
                    .unwrap_or(&NO_ALIAS_SET.to_string())
                    .clone()
                    .replace(|c: char| !c.is_ascii(), "?"),
                scid: *scid,
                pubkey: *scid_peer_map.get(scid).unwrap(),
                status: task_states.clone(),
                status_str: task_states.join("\n"),
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
        if json_summary {
            Ok(json!(table))
        } else {
            let tabled = Table::new(table);
            Ok(json!({"format-hint":"simple","result":format!("{}", tabled,)}))
        }
    }
}

fn success_stats(
    successes: Vec<SuccessReb>,
    time_window: u64,
    alias_map: &HashMap<PublicKey, String>,
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
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
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
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
                    .unwrap_or(&"ALIAS_NOT_FOUND".to_string())
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
    peer_channels: &HashMap<ShortChannelId, ListpeerchannelsChannels>,
    partner: &ShortChannelId,
    alias_map: &HashMap<PublicKey, String>,
) -> String {
    if let Some(chan) = &peer_channels.get(partner) {
        if let Some(alias) = alias_map.get(&chan.peer_id) {
            alias.clone()
        } else {
            "ALIAS_NOT_FOUND".to_string()
        }
    } else {
        "PEER_NOT_FOUND".to_string()
    }
}
