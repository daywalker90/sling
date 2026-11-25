use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use cln_rpc::{
    model::responses::GetinfoResponse,
    primitives::{Amount, PublicKey, ShortChannelId, ShortChannelIdDir},
};
use sling::Job;

use crate::{
    dijkstra::dijkstra,
    gossip::read_gossip_file,
    model::{Config, IncompleteChannels, JobMessage, LnGraph, PLUGIN_NAME, Task},
    util::feeppm_effective,
};

#[test]
fn test_effective_feeppm() {
    assert_eq!(feeppm_effective(0, 0, 1_000), 0);
    assert_eq!(feeppm_effective(0, 0, 200_000), 0);
    assert_eq!(feeppm_effective(0, 0, 9_999_999_999), 0);

    assert_eq!(feeppm_effective(1, 0, 1_000), 1);
    assert_eq!(feeppm_effective(68, 0, 1_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 1_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 1_000), 1_000);
    assert_eq!(feeppm_effective(0, 53, 1_000), 53_000);
    assert_eq!(feeppm_effective(0, 10_009_000, 1_000), 10_009_000_000);

    assert_eq!(feeppm_effective(1, 0, 200_000), 1);
    assert_eq!(feeppm_effective(68, 0, 200_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 200_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 200_000), 5);
    assert_eq!(feeppm_effective(0, 53, 200_000), 265);
    assert_eq!(feeppm_effective(0, 10_009_000, 200_000), 50_045_000);

    assert_eq!(feeppm_effective(100, 0, 9_999_999_999), 100);
    assert_eq!(feeppm_effective(68, 0, 9_999_999_999), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 9_999_999_999), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 53, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 10_009_000, 9_999_999_999), 1_001);

    assert_eq!(feeppm_effective(1, 1, 1_000), 1_001);
    assert_eq!(feeppm_effective(68, 53, 1_000), 53_068);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 1_000),
        10_020_115_555
    );

    assert_eq!(feeppm_effective(1, 1, 200_000), 6);
    assert_eq!(feeppm_effective(68, 53, 200_000), 333);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 200_000),
        61_160_555
    );

    assert_eq!(feeppm_effective(1, 1, 9_999_999_999), 2);
    assert_eq!(feeppm_effective(68, 53, 9_999_999_999), 69);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 9_999_999_999),
        11_116_556
    );

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 1_000),
        4_299_262_262_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 1_000), 4_294_967_295_000);
    assert_eq!(feeppm_effective(u32::MAX, 0, 1_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 200_000),
        25_769_803_770
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 200_000), 21_474_836_475);
    assert_eq!(feeppm_effective(u32::MAX, 0, 200_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 9_999_999_999),
        4_295_396_792
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 9_999_999_999), 429_497);
    assert_eq!(feeppm_effective(u32::MAX, 0, 9_999_999_999), 4_294_967_296);

    assert_eq!(feeppm_effective(0, 0, u64::MAX), 0);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, u64::MAX),
        4_294_967_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, u64::MAX), 1);
    assert_eq!(feeppm_effective(u32::MAX, 0, u64::MAX), 4_294_967_296);
}

#[test]
fn test_fee_total() {
    use crate::util::fee_total_msat_precise;
    assert_eq!(fee_total_msat_precise(0, 0, 1_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 1_000).ceil() as u64, 1);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 1_000).ceil() as u64,
        4_294_968
    );

    assert_eq!(fee_total_msat_precise(0, 1, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 1_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 1_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 1_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 1_000).ceil() as u64,
        1_235
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 1_000).ceil() as u64,
        4_299_262_263
    );

    assert_eq!(fee_total_msat_precise(0, 0, 200_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 200_000).ceil() as u64, 70);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 200_000).ceil() as u64,
        858_993_460
    );

    assert_eq!(fee_total_msat_precise(0, 1, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 200_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 200_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 200_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 200_000).ceil() as u64,
        1_304
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 200_000).ceil() as u64,
        5_153_960_754
    );
}

#[test]
fn test_feeppm_effective_from_amts() {
    use crate::util::feeppm_effective_from_amts;
    assert_eq!(feeppm_effective_from_amts(1_000, 1_000), 0);
    assert_eq!(feeppm_effective_from_amts(u64::MAX, u64::MAX), 0);

    assert_eq!(feeppm_effective_from_amts(200_000_200, 200_000_000), 1);
    assert_eq!(feeppm_effective_from_amts(201_001_234, 201_000_000), 7);
    assert_eq!(feeppm_effective_from_amts(100_125_200, 100_000_000), 1_252);

    let result1 = std::panic::catch_unwind(|| feeppm_effective_from_amts(1_000, 2_000));
    assert!(result1.is_err());
}

fn make_test_config(node: PublicKey) -> Config {
    let getinfo = GetinfoResponse {
        lightning_dir: String::new(),
        alias: "test".to_owned(),
        our_features: None,
        warning_bitcoind_sync: None,
        warning_lightningd_sync: None,
        address: Vec::new(),
        binding: None,
        blockheight: 100,
        color: String::new(),
        fees_collected_msat: Amount::from_msat(0),
        id: node,
        network: "regtest".to_owned(),
        num_active_channels: 1,
        num_inactive_channels: 0,
        num_peers: 1,
        num_pending_channels: 0,
        version: "v25.02".to_owned(),
    };
    Config::new(
        getinfo,
        PathBuf::from("lightning-rpc"),
        PathBuf::from(PLUGIN_NAME),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    )
}

#[test]
fn test_parse_job_target_validation() {
    use crate::parse::parse_job;
    use serde_json::json;

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let node =
        PublicKey::from_str("0221299f99b760f35851a3a6ae60f62680b79ec02a34c3b25f6ba1e0ce9da62ba7")
            .unwrap();
    let config = make_test_config(node);

    // Out-of-range targets must be rejected (e.g. passing percentage 50 instead of 0.5)
    for bad_target in [json!(50), json!(1.01), json!(-0.1)] {
        let res = rt.block_on(parse_job(
            json!({
                "scid": "100x1x0",
                "direction": "pull",
                "amount": 10_000,
                "maxppm": 500,
                "outppm": 500,
                "target": bad_target,
            }),
            &config,
        ));
        assert!(res.is_err(), "expected error for target={bad_target}");
    }

    // Valid targets in [0.0, 1.0] must succeed
    for good_target in [json!(0.0), json!(0.5), json!(1.0), json!(1)] {
        let res = rt.block_on(parse_job(
            json!({
                "scid": "100x1x0",
                "direction": "pull",
                "amount": 10_000,
                "maxppm": 500,
                "outppm": 500,
                "target": good_target,
            }),
            &config,
        ));
        assert!(res.is_ok(), "expected ok for target={good_target}: {res:?}");
    }
}

#[test]
fn test_parse_once_job_overflow() {
    use crate::parse::parse_once_job;
    use serde_json::json;

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let node =
        PublicKey::from_str("0221299f99b760f35851a3a6ae60f62680b79ec02a34c3b25f6ba1e0ce9da62ba7")
            .unwrap();
    let config = make_test_config(node);
    let huge = u64::MAX / 1_000 + 1;

    // amount overflow
    let res = rt.block_on(parse_once_job(
        json!({
            "scid": "100x1x0",
            "direction": "pull",
            "amount": huge,
            "onceamount": huge,
            "maxppm": 500,
            "outppm": 500,
        }),
        &config,
    ));
    assert!(res.is_err());

    // onceamount overflow
    let res = rt.block_on(parse_once_job(
        json!({
            "scid": "100x1x0",
            "direction": "pull",
            "amount": 1_000,
            "onceamount": huge,
            "maxppm": 500,
            "outppm": 500,
        }),
        &config,
    ));
    assert!(res.is_err());

    // depleteuptoamount overflow
    let res = rt.block_on(parse_once_job(
        json!({
            "scid": "100x1x0",
            "direction": "pull",
            "amount": 1_000,
            "onceamount": 2_000,
            "maxppm": 500,
            "outppm": 500,
            "depleteuptoamount": huge,
        }),
        &config,
    ));
    assert!(res.is_err());
}

fn make_pubkey(seed: u8) -> PublicKey {
    use bitcoin::secp256k1::{Secp256k1, SecretKey};
    let secp = Secp256k1::new();
    let mut bytes = [0u8; 32];
    bytes[31] = seed;
    let sk = SecretKey::from_slice(&bytes).unwrap();
    PublicKey::from_secret_key(&secp, &sk)
}

fn insert_edge(
    graph: &mut LnGraph,
    scid: ShortChannelId,
    source: PublicKey,
    destination: PublicKey,
    fee_ppm: u32,
    base_msat: u32,
    delay: u32,
    inbound_fee: Option<crate::gossip::InboundFee>,
) {
    use crate::model::{PubKeyBytes, ShortChannelIdDirState};
    let direction = if source < destination { 0 } else { 1 };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    graph.insert(
        ShortChannelIdDir {
            short_channel_id: scid,
            direction,
        },
        ShortChannelIdDirState {
            source: PubKeyBytes::from_pubkey(&source),
            destination: PubKeyBytes::from_pubkey(&destination),
            active: true,
            scid_alias: None,
            fee_per_millionth: fee_ppm,
            base_fee_millisatoshi: base_msat,
            htlc_maximum_msat: Amount::from_msat(10_000_000_000),
            htlc_minimum_msat: Amount::from_msat(1_000),
            delay,
            last_update: now,
            private: false,
            inbound_fee,
        },
    );
}

#[test]
fn test_dijkstra_build_route_extreme_fees_no_overflow_or_panic() {
    use crate::model::PubKeyBytes;
    use crate::util::feeppm_effective_from_amts;

    // 4-hop chain with u32::MAX ppm on every forwarding hop would overflow u64/i64
    // in old build_route and could panic in feeppm_effective_from_amts.
    let us = make_pubkey(1);
    let a = make_pubkey(2);
    let b = make_pubkey(3);
    let c = make_pubkey(4);
    let d = make_pubkey(5);

    let scid_ua = ShortChannelId::from_str("100x1x0").unwrap();
    let scid_ab = ShortChannelId::from_str("100x2x0").unwrap();
    let scid_bc = ShortChannelId::from_str("100x3x0").unwrap();
    let scid_cd = ShortChannelId::from_str("100x4x0").unwrap();
    let scid_du = ShortChannelId::from_str("100x5x0").unwrap();

    let mut graph = LnGraph::new();
    insert_edge(&mut graph, scid_ua, us, a, 0, 0, 34, None);
    insert_edge(&mut graph, scid_ab, a, b, u32::MAX, u32::MAX, 34, None);
    insert_edge(&mut graph, scid_bc, b, c, u32::MAX, u32::MAX, 34, None);
    insert_edge(&mut graph, scid_cd, c, d, u32::MAX, u32::MAX, 34, None);
    insert_edge(&mut graph, scid_du, d, us, u32::MAX, u32::MAX, 34, None);

    let mut config = make_test_config(us);
    config.cltv_delta = 34;
    config.cltv_final = 18;
    let job = Job::new(sling::SatDirection::Pull, 500_000_000, Some(1000), 1000);
    let mut task = Task::new(
        scid_du,
        1,
        JobMessage::Starting,
        false,
        PubKeyBytes::from_pubkey(&d),
    );

    let route = dijkstra(
        &config,
        &graph,
        &job,
        &mut task,
        &[scid_ua],
        &[],
        &HashMap::new(),
    )
    .unwrap();

    // Must either return empty route (fee exceeds u64::MAX) or a valid route where
    // amount_sent_msat >= amount_msat (never panicking or wrapping).
    if !route.is_empty() {
        let sent = route.first().unwrap().amount_msat.unwrap().msat();
        let recv = route.last().unwrap().amount_msat.unwrap().msat();
        assert!(sent >= recv);
        assert!(feeppm_effective_from_amts(sent, recv) > job.maxppm);
    }
}

#[test]
fn test_dijkstra_negative_inbound_fee_cannot_hide_upstream_outbound_fee() {
    use crate::gossip::InboundFee;
    use crate::model::PubKeyBytes;

    // Two paths from us -> dest:
    // Path 1 (trap): us -> a -> trap -> dest where a->trap charges 1_000_000 ppm outbound
    //   and trap advertises -1_000_000 ppm inbound on trap->a (cannot offset a's fee!).
    // Path 2 (cheap): us -> a -> good -> dest where every hop charges 50 ppm.
    let us = make_pubkey(1);
    let a = make_pubkey(2);
    let trap = make_pubkey(3);
    let good = make_pubkey(4);
    let dest = make_pubkey(5);

    let scid_ua = ShortChannelId::from_str("200x1x0").unwrap();
    let scid_at = ShortChannelId::from_str("200x2x0").unwrap();
    let scid_td = ShortChannelId::from_str("200x3x0").unwrap();
    let scid_ag = ShortChannelId::from_str("200x4x0").unwrap();
    let scid_gd = ShortChannelId::from_str("200x5x0").unwrap();
    let scid_du = ShortChannelId::from_str("200x6x0").unwrap();

    let mut graph = LnGraph::new();
    insert_edge(&mut graph, scid_ua, us, a, 0, 0, 34, None);
    insert_edge(&mut graph, scid_ua, a, us, 0, 0, 34, None);

    // Trap path: `a` charges huge outbound, `trap` advertises huge negative inbound on `trap->a`
    insert_edge(&mut graph, scid_at, a, trap, 1_000_000, 0, 34, None);
    insert_edge(
        &mut graph,
        scid_at,
        trap,
        a,
        0,
        0,
        34,
        Some(InboundFee {
            base_msat: 0,
            proportional_millionths: -1_000_000,
        }),
    );
    insert_edge(&mut graph, scid_td, trap, dest, 0, 0, 34, None);
    insert_edge(&mut graph, scid_td, dest, trap, 0, 0, 34, None);

    // Good path: 50 ppm per hop
    insert_edge(&mut graph, scid_ag, a, good, 50, 0, 34, None);
    insert_edge(&mut graph, scid_ag, good, a, 50, 0, 34, None);
    insert_edge(&mut graph, scid_gd, good, dest, 50, 0, 34, None);
    insert_edge(&mut graph, scid_gd, dest, good, 50, 0, 34, None);

    insert_edge(&mut graph, scid_du, dest, us, 10, 0, 34, None);

    let mut config = make_test_config(us);
    config.cltv_delta = 34;
    config.cltv_final = 18;
    let job = Job::new(sling::SatDirection::Pull, 100_000_000, Some(1000), 500);
    let mut task = Task::new(
        scid_du,
        1,
        JobMessage::Starting,
        false,
        PubKeyBytes::from_pubkey(&dest),
    );

    let route = dijkstra(
        &config,
        &graph,
        &job,
        &mut task,
        &[scid_ua],
        &[],
        &HashMap::new(),
    )
    .unwrap();

    assert!(!route.is_empty());
    // Must pick the genuinely cheap `good` path, NOT the `trap` path.
    assert!(route.iter().any(|h| h.channel == Some(scid_ag)));
    assert!(!route.iter().any(|h| h.channel == Some(scid_at)));
}

#[test]
fn test_dijkstra_push_first_hop_direction_inbound_fee() {
    use crate::gossip::InboundFee;
    use crate::model::PubKeyBytes;

    // Ensure `us > peer` so `us -> peer` has direction = 1 and `peer -> us` has direction = 0.
    // Previously construct_first_node hardcoded direction = 0, reading `us -> peer`'s inbound fee
    // instead of `peer -> us`'s inbound fee!
    let k1 = make_pubkey(1);
    let k2 = make_pubkey(2);
    let (peer, us) = if k1 < k2 { (k1, k2) } else { (k2, k1) };
    assert!(us > peer);
    let mid = make_pubkey(3);

    let scid_push = ShortChannelId::from_str("300x1x0").unwrap();
    let scid_pm = ShortChannelId::from_str("300x2x0").unwrap();
    let scid_mu = ShortChannelId::from_str("300x3x0").unwrap();

    let mut graph = LnGraph::new();
    // `us -> peer` (dir 1): we advertise a bogus inbound_fee (999_999 msat) that must NOT be used
    insert_edge(
        &mut graph,
        scid_push,
        us,
        peer,
        0,
        0,
        34,
        Some(InboundFee {
            base_msat: 999_999,
            proportional_millionths: 0,
        }),
    );
    // `peer -> us` (dir 0): peer charges 5_000 msat inbound fee on incoming HTLCs from us
    insert_edge(
        &mut graph,
        scid_push,
        peer,
        us,
        0,
        0,
        34,
        Some(InboundFee {
            base_msat: 5_000,
            proportional_millionths: 0,
        }),
    );
    // `peer -> mid`: 1_000 msat base outbound
    insert_edge(&mut graph, scid_pm, peer, mid, 0, 1_000, 34, None);
    insert_edge(&mut graph, scid_pm, mid, peer, 0, 0, 34, None);
    // `mid -> us`: 0 fee
    insert_edge(&mut graph, scid_mu, mid, us, 0, 0, 34, None);

    let mut config = make_test_config(us);
    config.cltv_delta = 34;
    config.cltv_final = 18;
    let amount = 100_000_000;
    let job = Job::new(sling::SatDirection::Push, amount, Some(0), 1000);
    let mut task = Task::new(
        scid_push,
        1,
        JobMessage::Starting,
        false,
        PubKeyBytes::from_pubkey(&peer),
    );

    let route = dijkstra(
        &config,
        &graph,
        &job,
        &mut task,
        &[scid_mu],
        &[],
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(route.len(), 3);
    // Expected total fee = peer's 1_000 msat outbound + peer's 5_000 msat inbound = 6_000 msat
    // (NOT 1_000 + 999_999!).
    assert_eq!(
        route.first().unwrap().amount_msat.unwrap().msat(),
        amount + 6_000
    );
}

#[test]
fn test_dijkstra_rejects_excessive_cltv_delay() {
    use crate::model::{MAX_HOP_CLTV_DELTA, MAX_ROUTE_CLTV_DELTA, PubKeyBytes};

    let us = make_pubkey(1);
    let high_cltv = make_pubkey(2);
    let safe_node = make_pubkey(3);
    let dest = make_pubkey(4);

    let scid_uh = ShortChannelId::from_str("400x1x0").unwrap();
    let scid_hd = ShortChannelId::from_str("400x2x0").unwrap();
    let scid_us = ShortChannelId::from_str("400x3x0").unwrap();
    let scid_sd = ShortChannelId::from_str("400x4x0").unwrap();
    let scid_du = ShortChannelId::from_str("400x5x0").unwrap();

    let mut graph = LnGraph::new();
    // 0-ppm path with malicious 1800-block CLTV delta (> MAX_HOP_CLTV_DELTA)
    insert_edge(&mut graph, scid_uh, us, high_cltv, 0, 0, 34, None);
    insert_edge(
        &mut graph,
        scid_hd,
        high_cltv,
        dest,
        0,
        0,
        MAX_HOP_CLTV_DELTA + 100,
        None,
    );

    // Safe path with 100 ppm and normal 40-block CLTV delta
    insert_edge(&mut graph, scid_us, us, safe_node, 0, 0, 34, None);
    insert_edge(&mut graph, scid_sd, safe_node, dest, 100, 0, 40, None);

    insert_edge(&mut graph, scid_du, dest, us, 10, 0, 34, None);

    let mut config = make_test_config(us);
    config.cltv_delta = 34;
    config.cltv_final = 18;
    let job = Job::new(sling::SatDirection::Pull, 100_000_000, Some(1000), 1000);
    let mut task = Task::new(
        scid_du,
        1,
        JobMessage::Starting,
        false,
        PubKeyBytes::from_pubkey(&dest),
    );

    let route = dijkstra(
        &config,
        &graph,
        &job,
        &mut task,
        &[scid_uh, scid_us],
        &[],
        &HashMap::new(),
    )
    .unwrap();

    assert!(!route.is_empty());
    assert!(route.iter().any(|h| h.channel == Some(scid_sd)));
    assert!(!route.iter().any(|h| h.channel == Some(scid_hd)));
    assert!(route.first().unwrap().delay.unwrap() <= MAX_ROUTE_CLTV_DELTA);
}

#[test]
fn test_dijkstra_rejects_cumulative_route_cltv_exceeding_cap() {
    use crate::model::{MAX_ROUTE_CLTV_DELTA, PubKeyBytes};

    // Each individual hop has delay 400 (<= MAX_HOP_CLTV_DELTA = 500),
    // but 3 hops sum to 34 + 34 + 400 + 400 + 400 = 1268 > MAX_ROUTE_CLTV_DELTA (1008).
    let us = make_pubkey(1);
    let n1 = make_pubkey(2);
    let n2 = make_pubkey(3);
    let n3 = make_pubkey(4);
    let dest = make_pubkey(5);

    let scid_u1 = ShortChannelId::from_str("500x1x0").unwrap();
    let scid_12 = ShortChannelId::from_str("500x2x0").unwrap();
    let scid_23 = ShortChannelId::from_str("500x3x0").unwrap();
    let scid_3d = ShortChannelId::from_str("500x4x0").unwrap();
    let scid_du = ShortChannelId::from_str("500x5x0").unwrap();

    let mut graph = LnGraph::new();
    insert_edge(&mut graph, scid_u1, us, n1, 0, 0, 34, None);
    insert_edge(&mut graph, scid_12, n1, n2, 0, 0, 400, None);
    insert_edge(&mut graph, scid_23, n2, n3, 0, 0, 400, None);
    insert_edge(&mut graph, scid_3d, n3, dest, 0, 0, 400, None);
    insert_edge(&mut graph, scid_du, dest, us, 0, 0, 34, None);

    let mut config = make_test_config(us);
    config.cltv_delta = 34;
    config.cltv_final = 18;
    let job = Job::new(sling::SatDirection::Pull, 100_000_000, Some(1000), 1000);
    let mut task = Task::new(
        scid_du,
        1,
        JobMessage::Starting,
        false,
        PubKeyBytes::from_pubkey(&dest),
    );

    let route = dijkstra(
        &config,
        &graph,
        &job,
        &mut task,
        &[scid_u1],
        &[],
        &HashMap::new(),
    )
    .unwrap();

    assert!(
        route.is_empty(),
        "route with cumulative delay > {MAX_ROUTE_CLTV_DELTA} should be rejected"
    );
}

#[test]
fn test_timeout_intermediate_channels_skips_own_chans_and_pairs_scid_correctly() {
    use crate::response::timeout_intermediate_channels;
    use crate::util::get_direction_from_nodes;
    use cln_rpc::model::requests::SendpayRoute;

    let us = make_pubkey(1);
    let peer_a = make_pubkey(2);
    let peer_b = make_pubkey(3);
    let peer_c = make_pubkey(4);

    let scid_first = ShortChannelId::from_str("600x1x0").unwrap();
    let scid_ab = ShortChannelId::from_str("600x2x0").unwrap();
    let scid_bc = ShortChannelId::from_str("600x3x0").unwrap();
    let scid_last = ShortChannelId::from_str("600x4x0").unwrap();

    let mk_hop = |id: PublicKey, channel: ShortChannelId| SendpayRoute {
        id: Some(id),
        channel: Some(channel),
        amount_msat: Some(Amount::from_msat(100_000)),
        delay: Some(34),
        amount_out_msat: None,
        cltv_out: None,
        node_id_out: None,
        short_channel_id_dir: None,
    };

    let route = vec![
        mk_hop(peer_a, scid_first),
        mk_hop(peer_b, scid_ab),
        mk_hop(peer_c, scid_bc),
        mk_hop(us, scid_last),
    ];

    let chans = timeout_intermediate_channels(&route, us).unwrap();
    assert_eq!(
        chans,
        vec![
            ShortChannelIdDir {
                short_channel_id: scid_ab,
                direction: get_direction_from_nodes(peer_a, peer_b).unwrap(),
            },
            ShortChannelIdDir {
                short_channel_id: scid_bc,
                direction: get_direction_from_nodes(peer_b, peer_c).unwrap(),
            },
        ]
    );
}

#[test]
fn test_waitsendpay_error_data_allows_optional_onion_fields() {
    use crate::errors::WaitsendpayErrorData;
    use serde_json::json;

    // PAY_UNPARSEABLE_ONION omits erring_channel, erring_node, failcode, failcodename, amount_msat
    let val = json!({
        "id": 1,
        "payment_hash": "0000000000000000000000000000000000000000000000000000000000000001",
        "status": "failed",
        "created_at": 1700000000,
        "amount_sent_msat": 100000
    });
    let parsed: WaitsendpayErrorData = serde_json::from_value(val).unwrap();
    assert!(parsed.erring_channel.is_none());
    assert!(parsed.erring_node.is_none());
    assert!(parsed.failcodename.is_none());
    assert!(parsed.amount_msat.is_none());
}

#[test]
fn test_parse_jobs_content_rejects_corrupt_json() {
    use crate::util::parse_jobs_content;

    assert!(parse_jobs_content("").unwrap().is_empty());
    assert!(parse_jobs_content("   \n  ").unwrap().is_empty());
    assert!(parse_jobs_content("{}").unwrap().is_empty());
    assert!(parse_jobs_content("{corrupt json").is_err());
}

// Read current RSS from /proc/self/statm (Linux-specific)
fn get_current_rss() -> Option<u64> {
    let mut file = File::open("/proc/self/statm").ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    let fields: Vec<&str> = contents.split_whitespace().collect();
    let resident_pages: u64 = fields.get(1)?.parse().ok()?;
    Some(resident_pages * 4096) // Convert pages to bytes (4KB page size)
}
#[test]
#[ignore = "requires a gossip_store file"]
fn test_gossip_file_reader() {
    let iterations = 5;
    let mut vec_times = Vec::new();
    let mut vec_after_times = Vec::new();
    let mut vec_rss = Vec::new();

    for i in 0..iterations {
        let peak_rss = Arc::new(AtomicU64::new(0));
        let sampling_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

        // Spawn a thread to sample RSS
        let peak_rss_clone = Arc::clone(&peak_rss);
        let sampling_active_clone = Arc::clone(&sampling_active);
        let sampling_handle = thread::spawn(move || {
            while sampling_active_clone.load(Ordering::Relaxed) {
                if let Some(rss) = get_current_rss() {
                    peak_rss_clone.fetch_max(rss, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(1)); // Sample every 1ms
            }
        });

        let gossip_file = File::open(Path::new("gossip_store")).expect("Failed to open file");
        let mut reader = BufReader::new(gossip_file);
        let mut is_start_up = true;
        let mut offset = 0;
        let mut graph = LnGraph::new();
        let mut incomplete_channels = IncompleteChannels::new();

        let now = Instant::now();
        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete_channels,
            &mut offset,
            None,
        )
        .expect("read_gossip_file failed");
        let elapsed = now.elapsed().as_millis();
        vec_times.push(elapsed);

        let now = Instant::now();
        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete_channels,
            &mut offset,
            None,
        )
        .expect("read_gossip_file failed");
        let elapsed_after = now.elapsed().as_millis();
        vec_after_times.push(elapsed_after);

        // Signal the sampling thread to stop
        sampling_active.store(false, Ordering::Relaxed);

        // Wait for the sampling thread to finish
        sampling_handle.join().expect("Sampling thread panicked");

        // Calculate peak RAM used
        let peak_rss = peak_rss.load(Ordering::Relaxed);
        println!(
            "Iteration {}: chans:{} nodes:{} incomplete_chans:{} time: {}ms after: {}ms RAM: {}MB",
            i + 1,
            graph.public_channel_count(),
            graph.node_count(),
            incomplete_channels.len(),
            elapsed,
            elapsed_after,
            peak_rss / (1024 * 1024)
        );
        vec_rss.push(peak_rss);
        File::create(Path::new("graph_refactor.json"))
            .unwrap()
            .write_all(&serde_json::to_vec_pretty(&graph).unwrap())
            .unwrap();
        File::create(Path::new("incomplete_graph_refactor.json"))
            .unwrap()
            .write_all(&serde_json::to_vec_pretty(&incomplete_channels).unwrap())
            .unwrap();
    }
    let vec_avg = vec_times.iter().sum::<u128>() / iterations as u128;
    let vec_after_avg = vec_after_times.iter().sum::<u128>() / iterations as u128;
    let rss_avg = vec_rss.iter().sum::<u64>() / iterations as u64;
    println!(
        "average: time {}ms, after {}ms, RAM: {}MB",
        vec_avg,
        vec_after_avg,
        rss_avg / (1024 * 1024)
    );
}

#[test]
#[ignore = "requires a gossip_store file"]
fn test_dijkstra_speed() {
    let iterations = 100;
    let mut vec_times = Vec::new();
    let mut vec_len = Vec::new();

    let peak_rss = Arc::new(AtomicU64::new(0));
    let sampling_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let peak_rss_clone = Arc::clone(&peak_rss);
    let sampling_active_clone = Arc::clone(&sampling_active);
    let sampling_handle = thread::spawn(move || {
        while sampling_active_clone.load(Ordering::Relaxed) {
            if let Some(rss) = get_current_rss() {
                peak_rss_clone.fetch_max(rss, Ordering::Relaxed);
            }
            thread::sleep(Duration::from_millis(1)); // Sample every 1ms
        }
    });

    let gossip_file = File::open(Path::new("gossip_store")).expect("Failed to open file");
    let mut reader = BufReader::new(gossip_file);
    let mut is_start_up = true;
    let mut offset = 0;
    let mut graph = LnGraph::new();
    let mut incomplete_channels = IncompleteChannels::new();
    read_gossip_file(
        &mut is_start_up,
        &mut reader,
        &mut graph,
        &mut incomplete_channels,
        &mut offset,
        None,
    )
    .expect("read_gossip_file failed");

    let start_node = crate::model::PubKeyBytes::from_str(
        "026165850492521f4ac8abd9bd8088123446d126f648ca35e60f88177dc149ceb2",
    )
    .unwrap();
    let rando_node =
        PublicKey::from_str("0221299f99b760f35851a3a6ae60f62680b79ec02a34c3b25f6ba1e0ce9da62ba7")
            .unwrap();

    let job = Job::new(sling::SatDirection::Pull, 50_000_000, Some(10000), 5000);
    let mut task = Task::new(
        ShortChannelId::from_str("852152x3048x0").unwrap(),
        1,
        JobMessage::Starting,
        false,
        crate::model::PubKeyBytes::from_str(
            "02b3a0d1df18df7121681e994170e8d000c9afc212301249c0fc011f330e141a31",
        )
        .unwrap(),
    );
    let two_weeks_ago = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 60 * 60 * 24 * 14) as u32;

    let getinfo = GetinfoResponse {
        lightning_dir: String::new(),
        alias: "test".to_owned(),
        our_features: None,
        warning_bitcoind_sync: None,
        warning_lightningd_sync: None,
        address: Vec::new(),
        binding: None,
        blockheight: 1,
        color: String::new(),
        fees_collected_msat: Amount::from_msat(0),
        id: rando_node,
        network: "regtest".to_owned(),
        num_active_channels: 1,
        num_inactive_channels: 0,
        num_peers: 1,
        num_pending_channels: 0,
        version: "v25.02".to_owned(),
    };

    let mut config = Config::new(
        getinfo,
        PathBuf::from("lightning-rpc"),
        PathBuf::from(PLUGIN_NAME),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );

    let candidatelist: Vec<ShortChannelId> = graph
        .edges(
            &start_node,
            two_weeks_ago,
            &[],
            &config,
            &job,
            &[],
            &HashMap::new(),
        )
        .iter()
        .map(|r| r.0.short_channel_id)
        .collect();
    println!("candidates: {}", candidatelist.len());
    config.pubkey_bytes = start_node;
    // task.actual_candidates = candidatelist;

    let mut excepts: Vec<ShortChannelIdDir> = Vec::new();

    println!("ready for profiling");
    sleep(Duration::from_secs(4));

    for _i in 0..iterations {
        let now = Instant::now();
        let route = dijkstra(
            &config,
            &graph,
            &job,
            &mut task,
            &candidatelist,
            &excepts,
            &HashMap::new(),
        )
        .unwrap();
        let elapsed = now.elapsed().as_millis();
        // for r in &route {
        //     println!(
        //         "{}/{}: route: {} {:4} {:17}",
        //         slingchan.short_channel_id,
        //         1,
        //         Amount::msat(&r.amount_msat),
        //         r.delay,
        //         r.channel.to_string(),
        //     );
        // }
        // println!("----------------------------------");
        if let Some(hop) = route.first() {
            excepts.push(ShortChannelIdDir {
                short_channel_id: hop.channel.unwrap(),
                direction: 0,
            });
            excepts.push(ShortChannelIdDir {
                short_channel_id: hop.channel.unwrap(),
                direction: 1,
            });
        }
        vec_len.push(route.len());
        vec_times.push(elapsed);
    }

    let vec_avg = vec_times.iter().sum::<u128>() / iterations as u128;
    let len_avg = vec_len.iter().sum::<usize>() / iterations as usize;

    // Signal the sampling thread to stop
    sampling_active.store(false, Ordering::Relaxed);

    // Wait for the sampling thread to finish
    sampling_handle.join().expect("Sampling thread panicked");

    // Calculate peak RAM used
    let peak_rss = peak_rss.load(Ordering::Relaxed);

    println!(
        "average: route_len:{} time {}ms, peak RAM: {}MB",
        len_avg,
        vec_avg,
        peak_rss / (1024 * 1024)
    );
}
