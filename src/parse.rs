use std::str::FromStr;

use anyhow::anyhow;
use cln_plugin::Error;
use cln_rpc::primitives::ShortChannelId;
use sling::{Job, SatDirection};

pub async fn parse_job(args: serde_json::Value) -> Result<(ShortChannelId, Job), Error> {
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

            let target = match ar.get("target") {
                Some(t) => Some(
                    t.as_f64()
                        .ok_or(anyhow!("target must be a floating point"))?,
                ),
                None => None,
            };

            let maxhops = match ar.get("maxhops") {
                Some(h) => Some(h.as_u64().ok_or(anyhow!("maxhops must be an integer"))? as u8),
                None => None,
            };
            if let Some(h) = maxhops {
                if h < 2 {
                    return Err(anyhow!("maxhops must be atleast 2"));
                }
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
            }

            let depleteuptoamount = match ar.get("depleteuptoamount") {
                Some(h) => Some(
                    h.as_u64()
                        .ok_or(anyhow!("depleteuptoamount must be an integer"))?
                        * 1_000,
                ),
                None => None,
            };

            let paralleljobs = match ar.get("paralleljobs") {
                Some(h) => Some(
                    h.as_u64()
                        .ok_or(anyhow!("paralleljobs must be an integer"))?
                        as u8,
                ),
                None => None,
            };
            if let Some(h) = paralleljobs {
                if h < 1 {
                    return Err(anyhow!("paralleljobs must be atleast 1"));
                }
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
                    "Atleast one of outppm and candidatelist need to be set."
                ));
            }
            Ok((
                chan_id,
                Job {
                    sat_direction,
                    amount_msat,
                    outppm,
                    maxppm,
                    candidatelist,
                    target,
                    maxhops,
                    depleteuptopercent,
                    depleteuptoamount,
                    paralleljobs,
                },
            ))
        }
        other => Err(anyhow!("Invalid arguments: {}", other.to_string())),
    }
}
