use anyhow::{anyhow, Error};
use chrono::Utc;
use cln_plugin::{
    options::{self},
    ConfiguredPlugin,
    Plugin,
};
use cln_rpc::{model::requests::ListconfigsRequest, ClnRpc, RpcError};
use serde_json::json;

use crate::{
    at_or_above_version,
    model::PluginState,
    Config,
    OPT_CANDIDATES_MIN_AGE,
    OPT_DEPLETEUPTOAMOUNT,
    OPT_DEPLETEUPTOPERCENT,
    OPT_INFORM_LAYERS,
    OPT_MAXHOPS,
    OPT_MAX_HTLC_COUNT,
    OPT_PARALLELJOBS,
    OPT_REFRESH_ALIASMAP_INTERVAL,
    OPT_RESET_LIQUIDITY_INTERVAL,
    OPT_STATS_DELETE_FAILURES_AGE,
    OPT_STATS_DELETE_FAILURES_SIZE,
    OPT_STATS_DELETE_SUCCESSES_AGE,
    OPT_STATS_DELETE_SUCCESSES_SIZE,
    OPT_TIMEOUTPAY,
};

pub async fn setconfig_callback(
    plugin: Plugin<PluginState>,
    args: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let name = args
        .get("config")
        .ok_or_else(|| anyhow!("Bad CLN object. No option name found!"))?
        .as_str()
        .ok_or_else(|| anyhow!("Bad CLN object. Option name not a string!"))?;
    let value = args
        .get("val")
        .ok_or_else(|| anyhow!("Bad CLN object. No value found for option: {name}"))?;

    let opt_value = parse_option(name, value).map_err(|e| {
        anyhow!(json!(RpcError {
            code: Some(-32602),
            message: e.to_string(),
            data: None
        }))
    })?;

    let mut config = plugin.state().config.lock();

    check_option(&mut config, name, &opt_value).map_err(|e| {
        anyhow!(json!(RpcError {
            code: Some(-32602),
            message: e.to_string(),
            data: None
        }))
    })?;

    plugin.set_option_str(name, opt_value).map_err(|e| {
        anyhow!(json!(RpcError {
            code: Some(-32602),
            message: e.to_string(),
            data: None
        }))
    })?;

    Ok(json!({}))
}

fn parse_option(name: &str, value: &serde_json::Value) -> Result<options::Value, Error> {
    match name {
        n if n.eq(OPT_DEPLETEUPTOPERCENT) => {
            if value.is_string() {
                Ok(options::Value::String(value.as_str().unwrap().to_owned()))
            } else {
                Err(anyhow!("{} is not a valid string!", name))
            }
        }
        // n if n.eq(OPT_INFORM_LAYERS) => {
        //     if let Some(layers) = value.as_array() {
        //         let mut string_array = Vec::new();
        //         for layer in layers.iter() {
        //             if layer.is_string() {
        //                 string_array.push(layer.as_str().unwrap().to_owned());
        //             } else {
        //                 return Err(anyhow!("{} is not a valid string!", layer));
        //             }
        //         }
        //         Ok(options::Value::StringArray(string_array))
        //     } else {
        //         Err(anyhow!("{} is not a valid string array!", name))
        //     }
        // }
        _ => {
            if let Some(n_i64) = value.as_i64() {
                return Ok(options::Value::Integer(n_i64));
            } else if let Some(n_str) = value.as_str() {
                if let Ok(n_neg_i64) = n_str.parse::<i64>() {
                    return Ok(options::Value::Integer(n_neg_i64));
                }
            }
            Err(anyhow!("{} is not a valid integer!", name))
        }
    }
}

fn validate_u64_input(
    value: u64,
    var_name: &str,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    if value < gteq {
        return Err(anyhow!(
            "{} must be greater than or equal to {}",
            var_name,
            gteq
        ));
    }

    if let Some(factor) = time_factor_to_secs {
        if !is_valid_hour_timestamp(value.saturating_mul(factor)) {
            return Err(anyhow!(
                "{} needs to be a positive number and smaller than {}, \
            not `{}`.",
                var_name,
                (Utc::now().timestamp() as u64),
                value
            ));
        }
    }

    Ok(value)
}

fn is_valid_hour_timestamp(val: u64) -> bool {
    Utc::now().timestamp() as u64 > val
}

fn options_value_to_u64(
    name: &str,
    value: i64,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    if value >= 0 {
        validate_u64_input(value as u64, name, gteq, time_factor_to_secs)
    } else {
        Err(anyhow!(
            "{} needs to be a positive number and not `{}`.",
            name,
            value
        ))
    }
}

pub async fn get_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let rpc_path = state.config.lock().rpc_path.clone();
    let mut rpc = ClnRpc::new(rpc_path).await?;
    let cltv_delta = rpc
        .call_typed(&ListconfigsRequest {
            config: Some("cltv-delta".to_string()),
        })
        .await?
        .configs
        .ok_or_else(|| anyhow!("no configs found?! This should not happen!"))?
        .cltv_delta
        .ok_or_else(|| anyhow!("no cltv-delta config found?! This should not happen!"))?
        .value_int;

    let mut config = state.config.lock();
    config.cltv_delta = cltv_delta;
    config.at_or_above_24_11 = at_or_above_version(&config.version, "24.11")?;

    if let Some(rai) = plugin.option_str(OPT_REFRESH_ALIASMAP_INTERVAL)? {
        check_option(&mut config, OPT_REFRESH_ALIASMAP_INTERVAL, &rai)?;
    };
    if let Some(rli) = plugin.option_str(OPT_RESET_LIQUIDITY_INTERVAL)? {
        check_option(&mut config, OPT_RESET_LIQUIDITY_INTERVAL, &rli)?;
    };
    if let Some(dup) = plugin.option_str(OPT_DEPLETEUPTOPERCENT)? {
        check_option(&mut config, OPT_DEPLETEUPTOPERCENT, &dup)?;
    };
    if let Some(dua) = plugin.option_str(OPT_DEPLETEUPTOAMOUNT)? {
        check_option(&mut config, OPT_DEPLETEUPTOAMOUNT, &dua)?;
    };
    if let Some(mhops) = plugin.option_str(OPT_MAXHOPS)? {
        check_option(&mut config, OPT_MAXHOPS, &mhops)?;
    };
    if let Some(cma) = plugin.option_str(OPT_CANDIDATES_MIN_AGE)? {
        check_option(&mut config, OPT_CANDIDATES_MIN_AGE, &cma)?;
    };
    if let Some(pj) = plugin.option_str(OPT_PARALLELJOBS)? {
        check_option(&mut config, OPT_PARALLELJOBS, &pj)?;
    };
    if let Some(tp) = plugin.option_str(OPT_TIMEOUTPAY)? {
        check_option(&mut config, OPT_TIMEOUTPAY, &tp)?;
    };
    if let Some(mhc) = plugin.option_str(OPT_MAX_HTLC_COUNT)? {
        check_option(&mut config, OPT_MAX_HTLC_COUNT, &mhc)?;
    };
    if let Some(sdfa) = plugin.option_str(OPT_STATS_DELETE_FAILURES_AGE)? {
        check_option(&mut config, OPT_STATS_DELETE_FAILURES_AGE, &sdfa)?;
    };
    if let Some(sdfs) = plugin.option_str(OPT_STATS_DELETE_FAILURES_SIZE)? {
        check_option(&mut config, OPT_STATS_DELETE_FAILURES_SIZE, &sdfs)?;
    };
    if let Some(sdsa) = plugin.option_str(OPT_STATS_DELETE_SUCCESSES_AGE)? {
        check_option(&mut config, OPT_STATS_DELETE_SUCCESSES_AGE, &sdsa)?;
    };
    if let Some(sdss) = plugin.option_str(OPT_STATS_DELETE_SUCCESSES_SIZE)? {
        check_option(&mut config, OPT_STATS_DELETE_SUCCESSES_SIZE, &sdss)?;
    };
    if let Some(layers) = plugin.option_str(OPT_INFORM_LAYERS)? {
        check_option(&mut config, OPT_INFORM_LAYERS, &layers)?;
    }

    Ok(())
}

fn check_option(config: &mut Config, name: &str, value: &options::Value) -> Result<(), Error> {
    match name {
        n if n.eq(OPT_REFRESH_ALIASMAP_INTERVAL) => {
            config.refresh_aliasmap_interval = options_value_to_u64(
                OPT_REFRESH_ALIASMAP_INTERVAL,
                value.as_i64().unwrap(),
                1,
                None,
            )?
        }
        n if n.eq(OPT_RESET_LIQUIDITY_INTERVAL) => {
            config.reset_liquidity_interval = options_value_to_u64(
                OPT_RESET_LIQUIDITY_INTERVAL,
                value.as_i64().unwrap(),
                10,
                None,
            )?
        }
        n if n.eq(OPT_DEPLETEUPTOPERCENT) => {
            config.depleteuptopercent = match value.as_str().unwrap().parse::<f64>() {
                Ok(f) => {
                    if (0.0..1.0).contains(&f) {
                        f
                    } else {
                        return Err(anyhow!(
                            "Error: {} needs to be greater than 0 and <1, not `{}`.",
                            OPT_DEPLETEUPTOPERCENT,
                            f
                        ));
                    }
                }
                Err(e) => {
                    return Err(anyhow!(
                        "Error: {} could not parse a floating point for `{}`.",
                        e,
                        OPT_DEPLETEUPTOPERCENT,
                    ))
                }
            }
        }
        n if n.eq(OPT_DEPLETEUPTOAMOUNT) => {
            config.depleteuptoamount =
                options_value_to_u64(OPT_DEPLETEUPTOAMOUNT, value.as_i64().unwrap(), 0, None)?
                    * 1000
        }
        n if n.eq(OPT_MAXHOPS) => {
            config.maxhops = u8::try_from(options_value_to_u64(
                OPT_MAXHOPS,
                value.as_i64().unwrap(),
                2,
                None,
            )?)?
        }
        n if n.eq(OPT_CANDIDATES_MIN_AGE) => {
            config.candidates_min_age = u32::try_from(options_value_to_u64(
                OPT_CANDIDATES_MIN_AGE,
                value.as_i64().unwrap(),
                0,
                None,
            )?)?
        }
        n if n.eq(OPT_PARALLELJOBS) => {
            config.paralleljobs = u16::try_from(options_value_to_u64(
                OPT_PARALLELJOBS,
                value.as_i64().unwrap(),
                1,
                None,
            )?)?
        }
        n if n.eq(OPT_TIMEOUTPAY) => {
            config.timeoutpay = u16::try_from(options_value_to_u64(
                OPT_TIMEOUTPAY,
                value.as_i64().unwrap(),
                1,
                None,
            )?)?
        }
        n if n.eq(OPT_MAX_HTLC_COUNT) => {
            config.max_htlc_count =
                options_value_to_u64(OPT_MAX_HTLC_COUNT, value.as_i64().unwrap(), 1, None)?
        }
        n if n.eq(OPT_STATS_DELETE_FAILURES_AGE) => {
            config.stats_delete_failures_age = options_value_to_u64(
                OPT_STATS_DELETE_FAILURES_AGE,
                value.as_i64().unwrap(),
                0,
                Some(24 * 60 * 60),
            )?
        }
        n if n.eq(OPT_STATS_DELETE_FAILURES_SIZE) => {
            config.stats_delete_failures_size = options_value_to_u64(
                OPT_STATS_DELETE_FAILURES_SIZE,
                value.as_i64().unwrap(),
                0,
                None,
            )?
        }
        n if n.eq(OPT_STATS_DELETE_SUCCESSES_AGE) => {
            config.stats_delete_successes_age = options_value_to_u64(
                OPT_STATS_DELETE_SUCCESSES_AGE,
                value.as_i64().unwrap(),
                0,
                Some(24 * 60 * 60),
            )?
        }
        n if n.eq(OPT_STATS_DELETE_SUCCESSES_SIZE) => {
            config.stats_delete_successes_size = options_value_to_u64(
                OPT_STATS_DELETE_SUCCESSES_SIZE,
                value.as_i64().unwrap(),
                0,
                None,
            )?
        }
        n if n.eq(OPT_INFORM_LAYERS) => {
            config.inform_layers = value.as_str_arr().unwrap().clone();
        }
        _ => return Err(anyhow!("Unknown option: {}", name)),
    }
    Ok(())
}
