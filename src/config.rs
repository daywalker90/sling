use anyhow::{anyhow, Error};
use chrono::Utc;
use cln_plugin::{
    options::{config_type::Integer, ConfigOption},
    ConfiguredPlugin,
};
use log::warn;
use parking_lot::Mutex;
use std::{path::Path, sync::Arc};

use tokio::fs;

use crate::{
    model::PluginState, Config, OPT_CANDIDATES_MIN_AGE, OPT_DEPLETEUPTOAMOUNT,
    OPT_DEPLETEUPTOPERCENT, OPT_MAXHOPS, OPT_MAX_HTLC_COUNT, OPT_PARALLELJOBS,
    OPT_REFRESH_ALIASMAP_INTERVAL, OPT_REFRESH_GRAPH_INTERVAL, OPT_REFRESH_PEERS_INTERVAL,
    OPT_RESET_LIQUIDITY_INTERVAL, OPT_STATS_DELETE_FAILURES_AGE, OPT_STATS_DELETE_FAILURES_SIZE,
    OPT_STATS_DELETE_SUCCESSES_AGE, OPT_STATS_DELETE_SUCCESSES_SIZE, OPT_TIMEOUTPAY, OPT_UTF8,
};

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
    opt: &ConfigOption<Integer>,
    value: i64,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    if value >= 0 {
        validate_u64_input(value as u64, opt.name, gteq, time_factor_to_secs)
    } else {
        Err(anyhow!(
            "{} needs to be a positive number and not `{}`.",
            opt.name,
            value
        ))
    }
}

fn str_to_u64(
    var_name: &str,
    value: &str,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    match value.parse::<u64>() {
        Ok(n) => validate_u64_input(n, var_name, gteq, time_factor_to_secs),
        Err(e) => Err(anyhow!(
            "Could not parse a positive number from `{}` for {}: {}",
            value,
            var_name,
            e
        )),
    }
}

pub async fn read_config(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let dir = plugin.configuration().lightning_dir;
    let general_configfile =
        match fs::read_to_string(Path::new(&dir).parent().unwrap().join("config")).await {
            Ok(file2) => file2,
            Err(_) => {
                warn!("No general config file found!");
                String::new()
            }
        };
    let network_configfile = match fs::read_to_string(Path::new(&dir).join("config")).await {
        Ok(file) => file,
        Err(_) => {
            warn!("No network config file found!");
            String::new()
        }
    };

    parse_config_file(general_configfile, state.config.clone())?;
    parse_config_file(network_configfile, state.config.clone())?;
    Ok(())
}

fn parse_config_file(configfile: String, config: Arc<Mutex<Config>>) -> Result<(), Error> {
    let mut config = config.lock();

    for line in configfile.lines() {
        if line.contains('=') {
            let splitline = line.split('=').collect::<Vec<&str>>();
            if splitline.len() == 2 {
                let name = splitline.first().unwrap();
                let value = splitline.get(1).unwrap();

                match name {
                    opt if opt.eq(&config.cltv_delta.name) => {
                        config.cltv_delta.value =
                            Some(str_to_u64(config.cltv_delta.name, value, 0, None)? as u16)
                    }
                    opt if opt.eq(&config.utf8.name) => match value.parse::<bool>() {
                        Ok(b) => config.utf8.value = b,
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse bool from `{}` for {}: {}",
                                value,
                                config.utf8.name,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.refresh_peers_interval.name) => {
                        config.refresh_peers_interval.value =
                            str_to_u64(config.refresh_peers_interval.name, value, 1, None)?
                    }
                    opt if opt.eq(&config.refresh_aliasmap_interval.name) => {
                        config.refresh_aliasmap_interval.value =
                            str_to_u64(config.refresh_aliasmap_interval.name, value, 1, None)?
                    }
                    opt if opt.eq(&config.refresh_graph_interval.name) => {
                        config.refresh_graph_interval.value =
                            str_to_u64(config.refresh_graph_interval.name, value, 1, None)?
                    }
                    opt if opt.eq(&config.reset_liquidity_interval.name) => {
                        config.reset_liquidity_interval.value =
                            str_to_u64(config.reset_liquidity_interval.name, value, 10, None)?
                    }
                    opt if opt.eq(&config.depleteuptopercent.name) => match value.parse::<f64>() {
                        Ok(n) => {
                            if (0.0..1.0).contains(&n) {
                                config.depleteuptopercent.value = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be between 0 and <1 for {}.",
                                    config.depleteuptopercent.name
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.depleteuptopercent.name,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.depleteuptoamount.name) => {
                        config.depleteuptoamount.value =
                            str_to_u64(config.depleteuptoamount.name, value, 0, None)? * 1000
                    }
                    opt if opt.eq(&config.maxhops.name) => {
                        config.maxhops.value =
                            str_to_u64(config.maxhops.name, value, 2, None)? as u8
                    }
                    opt if opt.eq(&config.candidates_min_age.name) => {
                        config.candidates_min_age.value =
                            str_to_u64(config.candidates_min_age.name, value, 0, None)? as u32
                    }
                    opt if opt.eq(&config.paralleljobs.name) => {
                        config.paralleljobs.value =
                            str_to_u64(config.paralleljobs.name, value, 1, None)? as u8
                    }
                    opt if opt.eq(&config.timeoutpay.name) => {
                        config.timeoutpay.value =
                            str_to_u64(config.timeoutpay.name, value, 1, None)? as u16
                    }
                    opt if opt.eq(&config.max_htlc_count.name) => {
                        config.max_htlc_count.value =
                            str_to_u64(config.max_htlc_count.name, value, 1, None)?
                    }
                    opt if opt.eq(&config.stats_delete_failures_age.name) => {
                        config.stats_delete_failures_age.value = str_to_u64(
                            config.stats_delete_failures_age.name,
                            value,
                            0,
                            Some(24 * 60 * 60),
                        )?
                    }
                    opt if opt.eq(&config.stats_delete_failures_size.name) => {
                        config.stats_delete_failures_size.value =
                            str_to_u64(config.stats_delete_failures_size.name, value, 0, None)?
                    }
                    opt if opt.eq(&config.stats_delete_successes_age.name) => {
                        config.stats_delete_successes_age.value = str_to_u64(
                            config.stats_delete_successes_age.name,
                            value,
                            0,
                            Some(24 * 60 * 60),
                        )?
                    }
                    opt if opt.eq(&config.stats_delete_successes_size.name) => {
                        config.stats_delete_successes_size.value =
                            str_to_u64(config.stats_delete_successes_size.name, value, 0, None)?
                    }
                    _ => (),
                }
            }
        }
    }
    Ok(())
}

pub fn get_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    if let Some(utf8) = plugin.option(&OPT_UTF8)? {
        config.utf8.value = utf8
    };
    if let Some(rpi) = plugin.option(&OPT_REFRESH_PEERS_INTERVAL)? {
        config.refresh_peers_interval.value =
            options_value_to_u64(&OPT_REFRESH_PEERS_INTERVAL, rpi, 1, None)?
    };
    if let Some(rai) = plugin.option(&OPT_REFRESH_ALIASMAP_INTERVAL)? {
        config.refresh_aliasmap_interval.value =
            options_value_to_u64(&OPT_REFRESH_ALIASMAP_INTERVAL, rai, 1, None)?
    };
    if let Some(rgi) = plugin.option(&OPT_REFRESH_GRAPH_INTERVAL)? {
        config.refresh_graph_interval.value =
            options_value_to_u64(&OPT_REFRESH_GRAPH_INTERVAL, rgi, 1, None)?
    };
    if let Some(rli) = plugin.option(&OPT_RESET_LIQUIDITY_INTERVAL)? {
        config.reset_liquidity_interval.value =
            options_value_to_u64(&OPT_RESET_LIQUIDITY_INTERVAL, rli, 10, None)?
    };
    if let Some(dup) = plugin.option(&OPT_DEPLETEUPTOPERCENT)? {
        config.depleteuptopercent.value = match dup.parse::<f64>() {
            Ok(f) => {
                if (0.0..1.0).contains(&f) {
                    f
                } else {
                    return Err(anyhow!(
                        "Error: {} needs to be greater than 0 and <1, not `{}`.",
                        config.depleteuptopercent.name,
                        f
                    ));
                }
            }
            Err(e) => {
                return Err(anyhow!(
                    "Error: {} could not parse a floating point for `{}`.",
                    e,
                    config.depleteuptopercent.name,
                ))
            }
        }
    };
    if let Some(dua) = plugin.option(&OPT_DEPLETEUPTOAMOUNT)? {
        config.depleteuptoamount.value =
            options_value_to_u64(&OPT_DEPLETEUPTOAMOUNT, dua, 0, None)? * 1000
    };
    if let Some(mhops) = plugin.option(&OPT_MAXHOPS)? {
        config.maxhops.value = options_value_to_u64(&OPT_MAXHOPS, mhops, 2, None)? as u8
    };
    if let Some(cma) = plugin.option(&OPT_CANDIDATES_MIN_AGE)? {
        config.candidates_min_age.value =
            options_value_to_u64(&OPT_CANDIDATES_MIN_AGE, cma, 0, None)? as u32
    };
    if let Some(pj) = plugin.option(&OPT_PARALLELJOBS)? {
        config.paralleljobs.value = options_value_to_u64(&OPT_PARALLELJOBS, pj, 1, None)? as u8
    };
    if let Some(tp) = plugin.option(&OPT_TIMEOUTPAY)? {
        config.timeoutpay.value = options_value_to_u64(&OPT_TIMEOUTPAY, tp, 1, None)? as u16
    };
    if let Some(mhc) = plugin.option(&OPT_MAX_HTLC_COUNT)? {
        config.max_htlc_count.value = options_value_to_u64(&OPT_MAX_HTLC_COUNT, mhc, 1, None)?
    };
    if let Some(sdfa) = plugin.option(&OPT_STATS_DELETE_FAILURES_AGE)? {
        config.stats_delete_failures_age.value =
            options_value_to_u64(&OPT_STATS_DELETE_FAILURES_AGE, sdfa, 0, Some(24 * 60 * 60))?
    };
    if let Some(sdfs) = plugin.option(&OPT_STATS_DELETE_FAILURES_SIZE)? {
        config.stats_delete_failures_size.value =
            options_value_to_u64(&OPT_STATS_DELETE_FAILURES_SIZE, sdfs, 0, None)?
    };
    if let Some(sdsa) = plugin.option(&OPT_STATS_DELETE_SUCCESSES_AGE)? {
        config.stats_delete_successes_age.value =
            options_value_to_u64(&OPT_STATS_DELETE_SUCCESSES_AGE, sdsa, 0, Some(24 * 60 * 60))?
    };
    if let Some(sdss) = plugin.option(&OPT_STATS_DELETE_SUCCESSES_SIZE)? {
        config.stats_delete_successes_size.value =
            options_value_to_u64(&OPT_STATS_DELETE_SUCCESSES_SIZE, sdss, 0, None)?
    };

    Ok(())
}
