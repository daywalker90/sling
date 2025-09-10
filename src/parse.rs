use std::str::FromStr;

use anyhow::anyhow;
use cln_plugin::Error;
use cln_rpc::primitives::ShortChannelId;
use sling::{Job, SatDirection};

use crate::model::Config;

pub async fn parse_job(
    args: serde_json::Value,
    config: &Config,
) -> Result<(ShortChannelId, Job), Error> {
    let valid_keys = [
        "scid",
        "direction",
        "amount",
        "maxppm",
        "outppm",
        "target",
        "maxhops",
        "candidates",
        "depleteuptopercent",
        "depleteuptoamount",
        "paralleljobs",
    ];

    match args {
        serde_json::Value::Object(ar) => {
            for k in ar.keys() {
                if !valid_keys.contains(&k.as_str()) {
                    return Err(anyhow!("Invalid argument: {}", k));
                }
            }

            let chan_id = match ar.get("scid") {
                Some(scid) => ShortChannelId::from_str(
                    scid.as_str().ok_or(anyhow!("invalid string for scid"))?,
                )?,
                None => return Err(anyhow!("Missing scid")),
            };

            let sat_direction = match ar.get("direction") {
                Some(dir) => SatDirection::from_str(
                    dir.as_str()
                        .ok_or(anyhow!("invalid string for direction"))?,
                )?,
                None => return Err(anyhow!("Missing direction")),
            };

            //also convert to msat
            let amount_msat = match ar.get("amount") {
                Some(amt) => {
                    amt.as_u64()
                        .ok_or(anyhow!("amount must be a positive integer"))?
                        * 1_000
                }
                None => return Err(anyhow!("Missing amount")),
            };
            if amount_msat == 0 {
                return Err(anyhow!("amount must be greater than 0"));
            }

            let maxppm = match ar.get("maxppm") {
                Some(ppm) => ppm.as_u64().ok_or(anyhow!("maxppm must be an integer"))? as u32,
                None => return Err(anyhow!("Missing maxppm")),
            };

            let outppm = match ar.get("outppm") {
                Some(o) => Some(o.as_u64().ok_or(anyhow!("outppm must be an integer"))?),
                None => None,
            };

            let mut job = Job::new(sat_direction, amount_msat, outppm, maxppm);

            if let Some(target) = ar.get("target") {
                job.add_target(
                    target
                        .as_f64()
                        .ok_or(anyhow!("target must be a floating point"))?,
                );
            };

            let maxhops = match ar.get("maxhops") {
                Some(h) => Some(h.as_u64().ok_or(anyhow!("maxhops must be an integer"))? as u8),
                None => None,
            };
            if let Some(h) = maxhops {
                if h < 2 {
                    return Err(anyhow!("maxhops must be atleast 2"));
                }
                job.add_maxhops(h);
            }

            let depleteuptopercent = match ar.get("depleteuptopercent") {
                Some(dp) => Some(
                    dp.as_f64()
                        .ok_or(anyhow!("depleteuptopercent must be a floating point"))?,
                ),
                None => None,
            };
            if let Some(dp) = depleteuptopercent {
                if !(0.0..1.0).contains(&dp) {
                    return Err(anyhow!("depleteuptopercent must be between 0.0 and <1.0"));
                }
                job.add_depleteuptopercent(dp);
            }

            if let Some(da) = ar.get("depleteuptoamount") {
                job.add_depleteuptoamount_msat(
                    da.as_u64()
                        .ok_or(anyhow!("depleteuptoamount must be an integer"))?
                        * 1_000,
                );
            };

            let paralleljobs = match ar.get("paralleljobs") {
                Some(h) => Some(
                    h.as_u64()
                        .ok_or(anyhow!("paralleljobs must be an integer"))?
                        as u16,
                ),
                None => None,
            };
            if let Some(pj) = paralleljobs {
                if pj < 1 {
                    return Err(anyhow!("paralleljobs must be atleast 1"));
                }
                if job.sat_direction == SatDirection::Push && pj > (config.max_htlc_count as u16) {
                    return Err(anyhow!(
                        "In a push job it doesn't make sense to have more \
                    paralleljobs than your max_htlc_count"
                    ));
                }
                job.add_paralleljobs(pj);
            }

            let candidatelist = {
                let mut tmpcandidatelist = Vec::new();
                match ar.get("candidates") {
                    Some(candidates) => {
                        for candidate in candidates
                            .as_array()
                            .ok_or(anyhow!("Invalid array for candidate list"))?
                        {
                            tmpcandidatelist.push(ShortChannelId::from_str(
                                candidate.as_str().ok_or(anyhow!(
                                    "invalid string for channel id in candidate list"
                                ))?,
                            )?);
                        }
                        Some(tmpcandidatelist)
                    }
                    None => None,
                }
            };
            if outppm.is_none() && candidatelist.is_none() {
                return Err(anyhow!(
                    "Atleast one of outppm and candidatelist must be set."
                ));
            }

            log::trace!(
                "candidatelist: {:?}",
                candidatelist.clone().map(|s| s
                    .iter()
                    .map(|sc| sc.to_string())
                    .collect::<Vec<String>>()
                    .join(", "))
            );
            log::trace!(
                "exclude_chans_pull: {}",
                config
                    .exclude_chans_pull
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            );
            log::trace!(
                "exclude_chans_push: {}",
                config
                    .exclude_chans_push
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            );

            if let Some(c) = candidatelist {
                for candidate in c.iter() {
                    match job.sat_direction {
                        SatDirection::Pull => {
                            if config.exclude_chans_pull.contains(candidate) {
                                return Err(anyhow!(
                                    "candidate {} has a pull-job that rebalances \
                                in the other direction!",
                                    candidate
                                ));
                            }
                        }
                        SatDirection::Push => {
                            if config.exclude_chans_push.contains(candidate) {
                                return Err(anyhow!(
                                    "candidate {} has a push-job that rebalances \
                                in the other direction!",
                                    candidate
                                ));
                            }
                        }
                    }
                }

                job.add_candidates(c);
            }

            Ok((chan_id, job))
        }
        other => Err(anyhow!("Expected an object! Got {} instead", other)),
    }
}

pub async fn parse_once_job(
    args: serde_json::Value,
    config: &Config,
) -> Result<(ShortChannelId, Job), Error> {
    let valid_keys = [
        "scid",
        "direction",
        "amount",
        "maxppm",
        "outppm",
        "target",
        "maxhops",
        "candidates",
        "depleteuptopercent",
        "depleteuptoamount",
        "paralleljobs",
        "onceamount",
    ];

    match args {
        serde_json::Value::Object(ar) => {
            for k in ar.keys() {
                if !valid_keys.contains(&k.as_str()) {
                    return Err(anyhow!("Invalid argument: {}", k));
                }
            }

            if ar.get("target").is_some() {
                return Err(anyhow!("`target` can not be used with `sling-once`"));
            }

            let chan_id = match ar.get("scid") {
                Some(scid) => ShortChannelId::from_str(
                    scid.as_str().ok_or(anyhow!("invalid string for scid"))?,
                )?,
                None => return Err(anyhow!("Missing scid")),
            };

            let sat_direction = match ar.get("direction") {
                Some(dir) => SatDirection::from_str(
                    dir.as_str()
                        .ok_or(anyhow!("invalid string for direction"))?,
                )?,
                None => return Err(anyhow!("Missing direction")),
            };

            //also convert to msat
            let amount_msat = match ar.get("amount") {
                Some(amt) => {
                    amt.as_u64()
                        .ok_or(anyhow!("amount must be a positive integer"))?
                        * 1_000
                }
                None => return Err(anyhow!("Missing amount")),
            };
            if amount_msat == 0 {
                return Err(anyhow!("amount must be greater than 0"));
            }

            let maxppm = match ar.get("maxppm") {
                Some(ppm) => ppm.as_u64().ok_or(anyhow!("maxppm must be an integer"))? as u32,
                None => return Err(anyhow!("Missing maxppm")),
            };

            let outppm = match ar.get("outppm") {
                Some(o) => Some(o.as_u64().ok_or(anyhow!("outppm must be an integer"))?),
                None => None,
            };

            let mut job = Job::new(sat_direction, amount_msat, outppm, maxppm);

            let onceamount_msat = match ar.get("onceamount") {
                Some(amt) => {
                    amt.as_u64()
                        .ok_or(anyhow!("onceamount must be a positive integer"))?
                        * 1_000
                }
                None => return Err(anyhow!("Missing onceamount")),
            };
            if onceamount_msat == 0 {
                return Err(anyhow!("onceamount must be greater than 0"));
            }
            if onceamount_msat % amount_msat != 0 {
                return Err(anyhow!(
                    "onceamount must be a multiple of amount. onceamount: {}, amount: {}",
                    onceamount_msat / 1000,
                    amount_msat / 1000
                ));
            }
            if onceamount_msat < amount_msat {
                return Err(anyhow!(
                    "onceamount must be greater than or equal to amount. \
                    onceamount: {}, amount: {}",
                    onceamount_msat / 1000,
                    amount_msat / 1000
                ));
            }
            job.add_onceamount_msat(onceamount_msat);

            let maxhops = match ar.get("maxhops") {
                Some(h) => Some(h.as_u64().ok_or(anyhow!("maxhops must be an integer"))? as u8),
                None => None,
            };
            if let Some(h) = maxhops {
                if h < 2 {
                    return Err(anyhow!("maxhops must be atleast 2"));
                }
                job.add_maxhops(h);
            }

            let depleteuptopercent = match ar.get("depleteuptopercent") {
                Some(dp) => Some(
                    dp.as_f64()
                        .ok_or(anyhow!("depleteuptopercent must be a floating point"))?,
                ),
                None => None,
            };
            if let Some(dp) = depleteuptopercent {
                if !(0.0..1.0).contains(&dp) {
                    return Err(anyhow!("depleteuptopercent must be between 0.0 and <1.0"));
                }
                job.add_depleteuptopercent(dp);
            }

            if let Some(da) = ar.get("depleteuptoamount") {
                job.add_depleteuptoamount_msat(
                    da.as_u64()
                        .ok_or(anyhow!("depleteuptoamount must be an integer"))?
                        * 1_000,
                );
            };

            let paralleljobs = match ar.get("paralleljobs") {
                Some(h) => Some(
                    h.as_u64()
                        .ok_or(anyhow!("paralleljobs must be an integer"))?
                        as u16,
                ),
                None => None,
            };
            if let Some(pj) = paralleljobs {
                if pj < 1 {
                    return Err(anyhow!("paralleljobs must be atleast 1"));
                }
                if job.sat_direction == SatDirection::Push && pj > (config.max_htlc_count as u16) {
                    return Err(anyhow!(
                        "In a push job it doesn't make sense to have more \
                    paralleljobs than your max_htlc_count"
                    ));
                }
                job.add_paralleljobs(pj);
            }

            let candidatelist = {
                let mut tmpcandidatelist = Vec::new();
                match ar.get("candidates") {
                    Some(candidates) => {
                        for candidate in candidates
                            .as_array()
                            .ok_or(anyhow!("Invalid array for candidate list"))?
                        {
                            tmpcandidatelist.push(ShortChannelId::from_str(
                                candidate.as_str().ok_or(anyhow!(
                                    "invalid string for channel id in candidate list"
                                ))?,
                            )?);
                        }
                        Some(tmpcandidatelist)
                    }
                    None => None,
                }
            };
            if outppm.is_none() && candidatelist.is_none() {
                return Err(anyhow!(
                    "Atleast one of outppm and candidatelist must be set."
                ));
            }

            log::trace!(
                "candidatelist: {:?}",
                candidatelist.clone().map(|s| s
                    .iter()
                    .map(|sc| sc.to_string())
                    .collect::<Vec<String>>()
                    .join(", "))
            );
            log::trace!(
                "exclude_chans_pull: {}",
                config
                    .exclude_chans_pull
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            );
            log::trace!(
                "exclude_chans_push: {}",
                config
                    .exclude_chans_push
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            );

            if let Some(c) = candidatelist {
                for candidate in c.iter() {
                    match job.sat_direction {
                        SatDirection::Pull => {
                            if config.exclude_chans_pull.contains(candidate) {
                                return Err(anyhow!(
                                    "candidate {} has a pull-job that rebalances \
                                in the other direction!",
                                    candidate
                                ));
                            }
                        }
                        SatDirection::Push => {
                            if config.exclude_chans_push.contains(candidate) {
                                return Err(anyhow!(
                                    "candidate {} has a push-job that rebalances \
                                in the other direction!",
                                    candidate
                                ));
                            }
                        }
                    }
                }
                job.add_candidates(c);
            }

            Ok((chan_id, job))
        }
        other => Err(anyhow!("Expected an object! Got {} instead", other)),
    }
}
