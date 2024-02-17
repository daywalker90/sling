use anyhow::{anyhow, Error};
use chrono::Utc;
use cln_plugin::{
    options::{config_type::DefaultInteger, ConfigOption},
    ConfiguredPlugin,
};
use log::{info, warn};
use std::path::Path;

use tokio::fs;

use crate::{
    model::PluginState,
    rpc::{get_config_path, get_info},
    OPT_CANDIDATES_MIN_AGE, OPT_DEPLETEUPTOAMOUNT, OPT_DEPLETEUPTOPERCENT, OPT_LIGHTNING_CONF,
    OPT_MAXHOPS, OPT_MAX_HTLC_COUNT, OPT_PARALLELJOBS, OPT_REFRESH_ALIASMAP_INTERVAL,
    OPT_REFRESH_GRAPH_INTERVAL, OPT_REFRESH_PEERS_INTERVAL, OPT_RESET_LIQUIDITY_INTERVAL,
    OPT_STATS_DELETE_FAILURES_AGE, OPT_STATS_DELETE_FAILURES_SIZE, OPT_STATS_DELETE_SUCCESSES_AGE,
    OPT_STATS_DELETE_SUCCESSES_SIZE, OPT_TIMEOUTPAY, OPT_UTF8,
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
    opt: &ConfigOption<DefaultInteger>,
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
    let mut config_file_content = String::new();
    let dir = plugin.configuration().lightning_dir;
    let rpc_path = Path::new(&dir).join(plugin.configuration().rpc_file);
    let getinfo = get_info(&rpc_path).await?;
    let config = state.config.lock().clone();
    let config_file_path = if config.lightning_conf.value.is_empty() {
        get_config_path(getinfo.lightning_dir).await?
    } else {
        vec![config.lightning_conf.value]
    };

    for confs in &config_file_path {
        match fs::read_to_string(Path::new(&confs)).await {
            Ok(f) => {
                info!("Found config file: {}", confs);
                config_file_content += &(f + "\n")
            }
            Err(e) => info!("Not a config file {}! {}", confs, e.to_string()),
        }
    }

    if config_file_content.is_empty() {
        warn!(
            "No config file found! Searched here: {}",
            config_file_path.join(", ")
        );
    }

    let mut config = state.config.lock();

    for line in config_file_content.lines() {
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

pub fn get_prestart_configs(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    config.lightning_conf.value = plugin.option(&OPT_LIGHTNING_CONF)?;

    Ok(())
}

pub fn get_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    config.utf8.value = plugin.option(&OPT_UTF8)?;
    config.refresh_peers_interval.value = options_value_to_u64(
        &OPT_REFRESH_PEERS_INTERVAL,
        plugin.option(&OPT_REFRESH_PEERS_INTERVAL)?,
        1,
        None,
    )?;
    config.refresh_aliasmap_interval.value = options_value_to_u64(
        &OPT_REFRESH_ALIASMAP_INTERVAL,
        plugin.option(&OPT_REFRESH_ALIASMAP_INTERVAL)?,
        1,
        None,
    )?;
    config.refresh_graph_interval.value = options_value_to_u64(
        &OPT_REFRESH_GRAPH_INTERVAL,
        plugin.option(&OPT_REFRESH_GRAPH_INTERVAL)?,
        1,
        None,
    )?;
    config.reset_liquidity_interval.value = options_value_to_u64(
        &OPT_RESET_LIQUIDITY_INTERVAL,
        plugin.option(&OPT_RESET_LIQUIDITY_INTERVAL)?,
        10,
        None,
    )?;
    config.depleteuptopercent.value = match plugin.option(&OPT_DEPLETEUPTOPERCENT)?.parse::<f64>() {
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
    };
    config.depleteuptoamount.value = options_value_to_u64(
        &OPT_DEPLETEUPTOAMOUNT,
        plugin.option(&OPT_DEPLETEUPTOAMOUNT)?,
        0,
        None,
    )? * 1000;
    config.maxhops.value =
        options_value_to_u64(&OPT_MAXHOPS, plugin.option(&OPT_MAXHOPS)?, 2, None)? as u8;
    config.candidates_min_age.value = options_value_to_u64(
        &OPT_CANDIDATES_MIN_AGE,
        plugin.option(&OPT_CANDIDATES_MIN_AGE)?,
        0,
        None,
    )? as u32;
    config.paralleljobs.value = options_value_to_u64(
        &OPT_PARALLELJOBS,
        plugin.option(&OPT_PARALLELJOBS)?,
        1,
        None,
    )? as u8;
    config.timeoutpay.value =
        options_value_to_u64(&OPT_TIMEOUTPAY, plugin.option(&OPT_TIMEOUTPAY)?, 1, None)? as u16;
    config.max_htlc_count.value = options_value_to_u64(
        &OPT_MAX_HTLC_COUNT,
        plugin.option(&OPT_MAX_HTLC_COUNT)?,
        1,
        None,
    )?;
    config.stats_delete_failures_age.value = options_value_to_u64(
        &OPT_STATS_DELETE_FAILURES_AGE,
        plugin.option(&OPT_STATS_DELETE_FAILURES_AGE)?,
        0,
        Some(24 * 60 * 60),
    )?;
    config.stats_delete_failures_size.value = options_value_to_u64(
        &OPT_STATS_DELETE_FAILURES_SIZE,
        plugin.option(&OPT_STATS_DELETE_FAILURES_SIZE)?,
        0,
        None,
    )?;
    config.stats_delete_successes_age.value = options_value_to_u64(
        &OPT_STATS_DELETE_SUCCESSES_AGE,
        plugin.option(&OPT_STATS_DELETE_SUCCESSES_AGE)?,
        0,
        Some(24 * 60 * 60),
    )?;
    config.stats_delete_successes_size.value = options_value_to_u64(
        &OPT_STATS_DELETE_SUCCESSES_SIZE,
        plugin.option(&OPT_STATS_DELETE_SUCCESSES_SIZE)?,
        0,
        None,
    )?;

    Ok(())
}
