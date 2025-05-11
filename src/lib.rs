use std::{
    collections::HashMap,
    fmt::{self, Display},
    str::FromStr,
};

use anyhow::{anyhow, Error};
use cln_rpc::{
    model::responses::ListpeerchannelsChannels,
    primitives::{Amount, PublicKey, ShortChannelId},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, Copy, PartialEq, Eq)]
pub enum SatDirection {
    #[serde(alias = "pull")]
    Pull,
    #[serde(alias = "push")]
    Push,
}

impl FromStr for SatDirection {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pull" => Ok(SatDirection::Pull),
            "push" => Ok(SatDirection::Push),
            _ => Err(anyhow!("could not parse flow direction from `{}`", s)),
        }
    }
}
impl fmt::Display for SatDirection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SatDirection::Pull => write!(f, "pull"),
            SatDirection::Push => write!(f, "push"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct Job {
    pub sat_direction: SatDirection,
    #[serde(alias = "amount")]
    pub amount_msat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outppm: Option<u64>,
    pub maxppm: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidatelist: Option<Vec<ShortChannelId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    maxhops: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depleteuptopercent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depleteuptoamount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    paralleljobs: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub once_amount_msat: Option<u64>,
}

impl Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("sat_direction:{}", self.sat_direction));
        parts.push(format!("amount_msat:{}", self.amount_msat));
        parts.push(format!("maxppm:{}", self.maxppm));

        if let Some(o) = self.outppm {
            parts.push(format!("outppm:{}", o));
        }
        if let Some(c) = &self.candidatelist {
            parts.push(format!(
                "candidatelist:{}",
                c.iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(",")
            ));
        }
        if let Some(t) = self.target {
            parts.push(format!("target:{}", t));
        }
        if let Some(m) = self.maxhops {
            parts.push(format!("maxhops:{}", m));
        }
        if let Some(d) = self.depleteuptopercent {
            parts.push(format!("depleteuptopercent:{}", d));
        }
        if let Some(d) = self.depleteuptoamount {
            parts.push(format!("depleteuptoamount:{}", d));
        }
        if let Some(p) = self.paralleljobs {
            parts.push(format!("paralleljobs:{}", p));
        }
        if let Some(t) = self.once_amount_msat {
            parts.push(format!("total_amount_msat:{}", t));
        }

        write!(f, "{}", parts.join(" "))
    }
}

impl Job {
    pub fn new(
        sat_direction: SatDirection,
        amount_msat: u64,
        outppm: Option<u64>,
        maxppm: u32,
    ) -> Job {
        Job {
            sat_direction,
            amount_msat,
            outppm,
            maxppm,
            candidatelist: None,
            target: None,
            maxhops: None,
            depleteuptopercent: None,
            depleteuptoamount: None,
            paralleljobs: None,
            once_amount_msat: None,
        }
    }
    pub fn add_candidates(&mut self, candidates: Vec<ShortChannelId>) {
        self.candidatelist = Some(candidates);
    }
    pub fn add_target(&mut self, target: f64) {
        self.target = Some(target);
    }
    pub fn add_maxhops(&mut self, maxhops: u8) {
        self.maxhops = Some(maxhops);
    }
    pub fn add_depleteuptopercent(&mut self, depleteuptopercent: f64) {
        self.depleteuptopercent = Some(depleteuptopercent);
    }
    pub fn add_depleteuptoamount(&mut self, depleteuptoamount: u64) {
        self.depleteuptoamount = Some(depleteuptoamount);
    }
    pub fn add_paralleljobs(&mut self, paralleljobs: u16) {
        self.paralleljobs = Some(paralleljobs);
    }
    pub fn add_once_amount_msat(&mut self, amount_msat: u64) {
        self.once_amount_msat = Some(amount_msat);
    }
    pub fn is_balanced(
        &self,
        channel: &ListpeerchannelsChannels,
        chan_id: &ShortChannelId,
    ) -> bool {
        let target_cap = self.target_cap(channel);
        if self.once_amount_msat.is_none() {
            log::debug!("{}: target: {}sats", chan_id, target_cap / 1_000);
        }

        let channel_msat = Amount::msat(&channel.total_msat.unwrap());
        let to_us_msat = Amount::msat(&channel.to_us_msat.unwrap());

        match self.sat_direction {
            SatDirection::Pull => to_us_msat >= target_cap,
            SatDirection::Push => channel_msat - to_us_msat >= target_cap,
        }
    }
    pub fn target_cap(&self, channel: &ListpeerchannelsChannels) -> u64 {
        let target = self.target.unwrap_or(0.5);

        let total_msat = Amount::msat(&channel.total_msat.unwrap());
        if self.once_amount_msat.is_some() {
            return total_msat;
        }
        let their_reserve_msat = Amount::msat(&channel.their_reserve_msat.unwrap());
        let our_reserve_msat = Amount::msat(&channel.our_reserve_msat.unwrap());

        let mut target_cap = (total_msat as f64 * target) as u64;
        match self.sat_direction {
            SatDirection::Pull => {
                if target_cap >= total_msat - their_reserve_msat - 1_000 {
                    target_cap = total_msat - their_reserve_msat - 2_000;
                }
            }
            SatDirection::Push => {
                if target_cap >= total_msat - our_reserve_msat - 1_000 {
                    target_cap = total_msat - our_reserve_msat - 2_000;
                }
            }
        }
        target_cap
    }
    pub fn to_json(&self) -> serde_json::Value {
        let mut result = HashMap::new();
        result.insert("direction", self.sat_direction.to_string());
        result.insert("amount", (self.amount_msat / 1_000).to_string());
        result.insert("maxppm", self.maxppm.to_string());
        match self.outppm {
            Some(o) => result.insert("outppm", o.to_string()),
            None => None,
        };
        match self.target {
            Some(t) => result.insert("target", t.to_string()),
            None => None,
        };
        match self.maxhops {
            Some(m) => result.insert("maxhops", m.to_string()),
            None => None,
        };
        match &self.candidatelist {
            Some(c) => result.insert(
                "candidates",
                c.iter()
                    .map(|y| y.to_string())
                    .collect::<Vec<String>>()
                    .join(", "),
            ),
            None => None,
        };
        match self.depleteuptopercent {
            Some(dp) => result.insert("depleteuptopercent", dp.to_string()),
            None => None,
        };
        match self.depleteuptoamount {
            Some(da) => result.insert("depleteuptoamount", (da / 1_000).to_string()),
            None => None,
        };
        match self.paralleljobs {
            Some(pj) => result.insert("paralleljobs", pj.to_string()),
            None => None,
        };
        json!(result)
    }
    pub fn get_maxhops(&self, config_maxhops: u8) -> u8 {
        if let Some(mh) = self.maxhops {
            mh + 1
        } else {
            config_maxhops + 1
        }
    }
    pub fn get_target(&self, config_target: f64) -> f64 {
        if let Some(t) = self.target {
            t
        } else {
            config_target
        }
    }
    pub fn get_deplteuptoamount(&self, config_depleteuptoamount: u64) -> u64 {
        if let Some(da) = self.depleteuptoamount {
            da
        } else {
            config_depleteuptoamount
        }
    }
    pub fn get_depleteuptopercent(&self, config_depleteuptopercent: f64) -> f64 {
        if let Some(dp) = self.depleteuptopercent {
            dp
        } else {
            config_depleteuptopercent
        }
    }
    pub fn get_paralleljobs(&self, config_paralleljobs: u16) -> u16 {
        if let Some(pj) = self.paralleljobs {
            pj
        } else {
            config_paralleljobs
        }
    }
    pub fn get_candidates(&self) -> Vec<ShortChannelId> {
        if let Some(c) = &self.candidatelist {
            c.clone()
        } else {
            Vec::new()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelPartnerStats {
    pub scid: ShortChannelId,
    pub alias: String,
    pub sats: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeerPartnerStats {
    pub peer_id: PublicKey,
    pub alias: String,
    pub count: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FailureReasonCount {
    pub failure_reason: String,
    pub failure_count: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FailuresInTimeWindow {
    pub time_window_days: String,
    pub total_amount_tried_sats: u64,
    pub top_5_failure_reasons: Vec<FailureReasonCount>,
    pub top_5_fail_nodes: Vec<PeerPartnerStats>,
    pub top_5_channel_partners: Vec<ChannelPartnerStats>,
    pub most_common_hop_count: Option<u8>,
    pub time_of_last_attempt: String,
    pub total_rebalances_tried: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SuccessesInTimeWindow {
    pub time_window_days: String,
    pub total_amount_sats: u64,
    pub feeppm_weighted_avg: u64,
    pub feeppm_min: u32,
    pub feeppm_max: u32,
    pub feeppm_median: u32,
    pub feeppm_90th_percentile: u32,
    pub top_5_channel_partners: Vec<ChannelPartnerStats>,
    pub most_common_hop_count: Option<u8>,
    pub time_of_last_rebalance: String,
    pub total_rebalances: u64,
    pub total_spent_sats: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SlingStats {
    pub successes_in_time_window: Option<SuccessesInTimeWindow>,
    pub failures_in_time_window: Option<FailuresInTimeWindow>,
}
