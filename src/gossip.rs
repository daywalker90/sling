use std::{
    fs::File,
    io::{BufReader, Read, Seek},
    time::Instant,
};

use anyhow::{Error, anyhow};
use bitcoin::consensus::encode::serialize_hex;
use cln_plugin::Plugin;
use cln_rpc::primitives::{Amount, ShortChannelId, ShortChannelIdDir};
use serde::Serialize;

use crate::{
    PluginState,
    model::{IncompleteChannels, LnGraph, PubKeyBytes, ShortChannelIdDirStateBuilder},
};

#[derive(Debug, Clone, Copy)]
pub struct ChannelUpdate {
    // pub direction: u32,
    // pub message_flags: u8,
    // pub channel_flags: u8,
    pub active: bool,
    pub last_update: u32,
    pub base_fee_millisatoshi: u32,
    pub fee_per_millionth: u32,
    pub delay: u32,
    pub htlc_minimum_msat: Amount,
    pub htlc_maximum_msat: Amount,
    pub inbound_fee: Option<InboundFee>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct InboundFee {
    pub base_msat: i32,
    pub proportional_millionths: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelAnnouncement {
    pub source: PubKeyBytes,
    pub destination: PubKeyBytes,
    // pub features: String,
}

#[derive(Debug, Clone, Copy)]
pub struct NextGossipStore {
    pub equivalent_offset: u64,
    pub uuid: [u8; 32],
    pub missed_compaction: bool,
}

const CHUNK_SIZE: usize = 1024 * 1024;

pub async fn read_gossip_store(
    plugin: Plugin<PluginState>,
    reader: &mut BufReader<File>,
    is_start_up: &mut bool,
    store_hint: Option<NextGossipStore>,
    reset_graph: bool,
) -> Result<Option<NextGossipStore>, Error> {
    let mut graph = plugin.state().graph.lock();
    let mut incomplete_channels = plugin.state().incomplete_channels.lock();

    if reset_graph {
        graph.clear();
        incomplete_channels.clear();
    }

    let mut offset = 0;

    let next_store = read_gossip_file(
        is_start_up,
        reader,
        &mut graph,
        &mut incomplete_channels,
        &mut offset,
        store_hint,
    )?;

    Ok(next_store)
}

pub fn read_gossip_file(
    is_start_up: &mut bool,
    reader: &mut BufReader<File>,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
    offset: &mut usize,
    store_hint: Option<NextGossipStore>,
) -> Result<Option<NextGossipStore>, anyhow::Error> {
    let now = Instant::now();

    if *is_start_up {
        // Read and check the version
        let mut gossip_ver_buffer = vec![0u8; 1];
        reader.read_exact(&mut gossip_ver_buffer)?;
        log::trace!("read_gossip_file: checking gossip_store version...");
        if (gossip_ver_buffer[0] & 0b1110_0000) != 0b0000_0000 {
            log::warn!("read_gossip_file: Unsupported gossip_store version!");
            return Err(anyhow!(
                "read_gossip_file: Unsupported gossip_store version!"
            ));
        }
        log::trace!("read_gossip_file: gossip_store version is good");
    }

    let mut gossip_file = vec![0u8; CHUNK_SIZE];

    let mut next_store = None;

    loop {
        let mut bytes_read = 0;

        // Read up to CHUNK_SIZE bytes
        while bytes_read < CHUNK_SIZE {
            match reader.read(&mut gossip_file[bytes_read..]) {
                Ok(0) => break, // EOF reached
                Ok(n) => bytes_read += n,
                Err(e) => return Err(e).map_err(|e| anyhow!("Error reading gossip file: {e}")),
            }
        }

        if bytes_read == 0 {
            break;
        }
        let test_now = Instant::now();

        next_store = read_gossip_file_chunk(
            &gossip_file[..bytes_read],
            offset,
            graph,
            incomplete_channels,
            reader,
            store_hint,
        )?;

        log::trace!(
            "read_gossip_file: gossip_store read chunk {} in: {}ms",
            bytes_read,
            test_now.elapsed().as_millis()
        );
        if next_store.is_some() {
            break;
        }
        if *offset < bytes_read {
            reader.seek_relative(*offset as i64 - bytes_read as i64)?;
            let should_break = *offset == 0 || bytes_read < CHUNK_SIZE;
            *offset = 0;
            if should_break {
                break;
            }
        } else {
            *offset = 0;
        }
    }
    log::trace!(
        "read_gossip_file: gossip_store read in: {}ms",
        now.elapsed().as_millis()
    );
    log::trace!(
        "read_gossip_file: found {} potential channels",
        incomplete_channels.len(),
    );

    let post_now = Instant::now();

    incomplete_channels.update_graph(graph);

    *is_start_up = false;

    log::trace!(
        "read_gossip_file: found {} actual channels and {} incomplete channels",
        graph.public_channel_count(),
        incomplete_channels.len()
    );
    log::trace!(
        "read_gossip_file: post_processing_time: {}ms",
        post_now.elapsed().as_millis()
    );
    Ok(next_store)
}

fn read_gossip_file_chunk(
    gossip_file: &[u8],
    offset: &mut usize,
    graph: &mut LnGraph,
    incomplete_channels: &mut IncompleteChannels,
    reader: &mut BufReader<File>,
    store_hint: Option<NextGossipStore>,
) -> Result<Option<NextGossipStore>, anyhow::Error> {
    log::trace!(
        "read_gossip_file_chunk: reading gossip_store chunk of size {}",
        gossip_file.len()
    );
    while *offset + 12 <= gossip_file.len() {
        // Read the 12-byte record header: flags (2), len (2), crc (4), timestamp (4)
        let flags = u16::from_be_bytes(gossip_file[*offset..*offset + 2].try_into()?);
        let len = u16::from_be_bytes(gossip_file[*offset + 2..*offset + 4].try_into()?) as usize;
        if *offset + 12 + len > gossip_file.len() {
            break;
        }
        *offset += 12;

        if len < 2 {
            log::warn!("read_gossip_file_chunk: record length {len} < 2, skipping");
            *offset += len;
            continue;
        }

        let msg_type = u16::from_be_bytes(gossip_file[*offset..*offset + 2].try_into()?);
        let body = &gossip_file[*offset + 2..*offset + len];
        *offset += len;

        // Check if the record is marked as deleted
        if flags & 0x8000 != 0 {
            continue;
        }
        // Check if the record is marked as dying
        if flags & 0x0800 != 0 {
            continue;
        }

        match msg_type {
            256 => {
                // public channel_announcement
                let (scid, chan_ann) = match parse_channel_announcement(body) {
                    Ok(x) => x,
                    Err(e) => {
                        log::warn!(
                            "read_gossip_file_chunk: Error parsing \
                            channel_announcement: {e}"
                        );
                        continue;
                    }
                };

                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                if !graph.has_announcement(&dir_chan_0, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_0) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_0.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_0, chan_state);
                    }
                }
                if !graph.has_announcement(&dir_chan_1, &chan_ann)? {
                    if let Some(chan_state) = incomplete_channels.get_mut(&dir_chan_1) {
                        if !chan_state.has_announcement() {
                            chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;
                        }
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_announcement(dir_chan_1.direction, chan_ann)?;

                        incomplete_channels.insert(dir_chan_1, chan_state);
                    }
                }
            }
            258 => {
                // channel_update
                let (scid_dir, chan_up) = match parse_channel_update(body) {
                    Ok(x) => x,
                    Err(e) => {
                        log::warn!(
                            "read_gossip_file_chunk: Error parsing \
                            channel_update: {e}"
                        );
                        continue;
                    }
                };

                let mut updated = false;

                if let Some(chan_state) = graph.get_state_mut_direction(scid_dir) {
                    chan_state.update(chan_up);
                    updated = true;
                }

                if !updated {
                    if let Some(chan_state) = incomplete_channels.get_mut(&scid_dir) {
                        chan_state.add_update(chan_up);
                    } else {
                        let mut chan_state = ShortChannelIdDirStateBuilder::new();
                        chan_state.add_update(chan_up);

                        incomplete_channels.insert(scid_dir, chan_state);
                    }
                }
            }
            4101 | 4102 | 4104 => {
                // 4101: gossip_store_channel_amount (unused)
                // 4102/4104: obsolete private channel records (modern CLN omits private
                // channels from gossip_store; local private channels are loaded from listpeerchannels)
            }
            4103 => {
                // 4103 gossip_store_delete_chan
                //  - `scid`: u64
                if body.len() < 8 {
                    log::warn!(
                        "read_gossip_file_chunk: truncated delete_chan ({} bytes)",
                        body.len()
                    );
                    continue;
                }
                let scid = match extract_scid(&body[0..8]) {
                    Ok(x) => x,
                    Err(e) => {
                        log::warn!("read_gossip_file_chunk: Error parsing scid: {e}");
                        continue;
                    }
                };

                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                graph.remove(&dir_chan_0);
                graph.remove(&dir_chan_1);
                incomplete_channels.remove(&dir_chan_0);
                incomplete_channels.remove(&dir_chan_1);
            }
            4106 => {
                // 4106 WIRE_GOSSIP_STORE_CHAN_DYING
                //  - `scid`: u64
                //  - `blockheight`: u32
                if body.len() < 12 {
                    log::warn!(
                        "read_gossip_file_chunk: truncated chan_dying ({} bytes)",
                        body.len()
                    );
                    continue;
                }
                let scid = match extract_scid(&body[0..8]) {
                    Ok(x) => x,
                    Err(e) => {
                        log::warn!("read_gossip_file_chunk: Error parsing scid: {e}");
                        continue;
                    }
                };

                let dir_chan_0 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 0,
                };
                let dir_chan_1 = ShortChannelIdDir {
                    short_channel_id: scid,
                    direction: 1,
                };
                graph.remove(&dir_chan_0);
                graph.remove(&dir_chan_1);
                incomplete_channels.remove(&dir_chan_0);
                incomplete_channels.remove(&dir_chan_1);
            }
            4105 => {
                // 4105 gossip_store_ended
                //  - `equivalent_offset`: u64
                //  - `uuid`: [u8; 32]
                if body.len() < 40 {
                    return Err(anyhow!(
                        "read_gossip_file_chunk: truncated gossip_store_ended ({} bytes)",
                        body.len()
                    ));
                }
                let equivalent_offset = u64::from_be_bytes(body[0..8].try_into()?);
                let uuid: [u8; 32] = body[8..40].try_into()?;
                log::trace!(
                    "read_gossip_file_chunk: encountered gossip_store_ended with \
                equivalent_offset: {equivalent_offset} and next uuid: {}",
                    serialize_hex(&uuid)
                );
                let next_store = NextGossipStore {
                    equivalent_offset,
                    uuid,
                    missed_compaction: false,
                };
                return Ok(Some(next_store));
            }
            4107 => {
                // 4107 uuid
                //  - `uuid`: [u8; 32]
                if body.len() < 32 {
                    return Err(anyhow!(
                        "read_gossip_file_chunk: truncated uuid record ({} bytes)",
                        body.len()
                    ));
                }
                let uuid: [u8; 32] = body[0..32].try_into()?;
                log::trace!(
                    "read_gossip_file_chunk: opened gossip_store with uuid: {}",
                    serialize_hex(&uuid)
                );

                if let Some(ns) = &store_hint {
                    if uuid != ns.uuid {
                        let next_store = NextGossipStore {
                            equivalent_offset: 0,
                            uuid,
                            missed_compaction: true,
                        };
                        log::info!(
                            "read_gossip_file_chunk: missed gossip_store compaction, \
                        reopening..."
                        );
                        return Ok(Some(next_store));
                    }
                    log::debug!(
                        "read_gossip_file_chunk: seeking to equivalent_offset {} in \
                    compacted gossip_store",
                        ns.equivalent_offset
                    );
                    reader.seek(std::io::SeekFrom::Start(ns.equivalent_offset))?;
                    *offset = gossip_file.len();
                    break;
                }
            }
            _e => {
                // Unknown message type
            }
        }
    }

    Ok(None)
}

fn extract_scid(gossip_file: &[u8]) -> Result<ShortChannelId, anyhow::Error> {
    let scid = u64::from_be_bytes(gossip_file.try_into()?);
    Ok(ShortChannelId::from(scid))
}

fn parse_channel_update(input: &[u8]) -> Result<(ShortChannelIdDir, ChannelUpdate), Error> {
    if input.len() < 136 {
        return Err(anyhow!("channel_update < 136 bytes"));
    }

    let scid = extract_scid(&input[96..104])?;

    let scid_dir = ShortChannelIdDir {
        short_channel_id: scid,
        direction: u32::from(input[109] & 0b0000_0001),
    };

    let mut inbound_fee = None;

    if input.len() > 136 {
        parse_channel_update_tlvs(&input[136..], &mut inbound_fee)?;
    }

    let update = ChannelUpdate {
        active: ((input[109] & 0b0000_0010) >> 1) != 1,
        last_update: u32::from_be_bytes(input[104..108].try_into()?),
        base_fee_millisatoshi: u32::from_be_bytes(input[120..124].try_into()?),
        fee_per_millionth: u32::from_be_bytes(input[124..128].try_into()?),
        delay: u32::from(u16::from_be_bytes(input[110..112].try_into()?)),
        htlc_minimum_msat: Amount::from_msat(u64::from_be_bytes(input[112..120].try_into()?)),
        htlc_maximum_msat: Amount::from_msat(u64::from_be_bytes(input[128..136].try_into()?)),

        inbound_fee,
    };

    Ok((scid_dir, update))
}

fn parse_channel_announcement(
    input: &[u8],
) -> Result<(ShortChannelId, ChannelAnnouncement), Error> {
    // 0..64 sig1
    // 64..128 sig2
    // 128..192 bc_sig_1
    // 192..256 bc_sig_2
    if input.len() < 258 {
        return Err(anyhow!("channel_announcement < 258 bytes"));
    }
    let len = u16::from_be_bytes(input[256..258].try_into()?) as usize;
    let required_len = 364usize
        .checked_add(len)
        .ok_or_else(|| anyhow!("channel_announcement length overflow"))?;
    if input.len() < required_len {
        return Err(anyhow!(
            "channel_announcement truncated: {} < {}",
            input.len(),
            required_len
        ));
    }
    // 258..258+len features
    // 258+len..290+len chain_hash
    let scid = extract_scid(&input[(290 + len)..(298 + len)])?;
    let source_bytes: [u8; 33] = input[(298 + len)..(331 + len)].try_into()?;
    let dest_bytes: [u8; 33] = input[(331 + len)..(364 + len)].try_into()?;
    if !matches!(source_bytes[0], 0x02 | 0x03) || !matches!(dest_bytes[0], 0x02 | 0x03) {
        return Err(anyhow!("channel_announcement has invalid compressed pubkey prefix"));
    }
    let source = PubKeyBytes::new(source_bytes);
    let destination = PubKeyBytes::new(dest_bytes);
    Ok((
        scid,
        ChannelAnnouncement {
            source,
            destination,
            // features: String::new(),
        },
    ))
}

fn parse_channel_update_tlvs(
    mut data: &[u8],
    inbound_fee: &mut Option<InboundFee>,
) -> Result<(), Error> {
    let mut last_type: Option<u64> = None;

    while !data.is_empty() {
        let (ty, rest) = read_bigsize(data)?;
        let (len, rest) = read_bigsize(rest)?;

        if last_type.is_some_and(|prev| ty <= prev) {
            return Err(anyhow!("TLV types not strictly increasing"));
        }

        last_type = Some(ty);

        let len_usize = usize::try_from(len).map_err(|_| anyhow!("TLV length overflow"))?;
        if rest.len() < len_usize {
            return Err(anyhow!("TLV truncated"));
        }

        let (value, remaining) = rest.split_at(len_usize);

        match ty {
            55555 => {
                if value.len() != 8 {
                    return Err(anyhow!("inbound_fee TLV length != 8"));
                }

                let base_msat = i32::from_be_bytes(value[0..4].try_into()?);
                let proportional_millionths = i32::from_be_bytes(value[4..8].try_into()?);

                if base_msat != 0 || proportional_millionths != 0 {
                    *inbound_fee = Some(InboundFee {
                        base_msat,
                        proportional_millionths,
                    });
                }
            }

            even if even % 2 == 0 => {
                return Err(anyhow!("unknown even TLV type {}", even));
            }

            _ => {
                // ignore unknown odd TLVs
            }
        }

        data = remaining;
    }

    Ok(())
}

fn read_bigsize(data: &[u8]) -> Result<(u64, &[u8]), Error> {
    if data.is_empty() {
        return Err(anyhow!("read_bigsize: data is empty"));
    }

    match data[0] {
        n @ 0x00..=0xfc => Ok((u64::from(n), &data[1..])),

        0xfd => {
            if data.len() < 3 {
                return Err(anyhow!("read_bigsize: data is <3"));
            }

            let v = u64::from(u16::from_be_bytes([data[1], data[2]]));

            if v < 0xfd {
                return Err(anyhow!("read_bigsize: v < 0xfd"));
            }

            Ok((v, &data[3..]))
        }

        0xfe => {
            if data.len() < 5 {
                return Err(anyhow!("read_bigsize: data is <5"));
            }

            let v = u64::from(u32::from_be_bytes(data[1..5].try_into().unwrap()));

            if v < 0x10000 {
                return Err(anyhow!("read_bigsize: v < 0x10000"));
            }

            Ok((v, &data[5..]))
        }

        0xff => {
            if data.len() < 9 {
                return Err(anyhow!("read_bigsize: data is <9"));
            }

            let v = u64::from_be_bytes(data[1..9].try_into().unwrap());

            if v < 0x1_0000_0000 {
                return Err(anyhow!("read_bigsize: v < 0x1_0000_0000"));
            }

            Ok((v, &data[9..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCase {
        name: &'static str,
        value: u64,
        bytes: &'static str,
        exp_error: Option<&'static str>,
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }

        assert!(s.len() % 2 == 0);

        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn test_read_bigsize_vectors() {
        let tests = vec![
            TestCase {
                name: "zero",
                value: 0,
                bytes: "00",
                exp_error: None,
            },
            TestCase {
                name: "one byte high",
                value: 252,
                bytes: "fc",
                exp_error: None,
            },
            TestCase {
                name: "two byte low",
                value: 253,
                bytes: "fd00fd",
                exp_error: None,
            },
            TestCase {
                name: "two byte high",
                value: 65535,
                bytes: "fdffff",
                exp_error: None,
            },
            TestCase {
                name: "four byte low",
                value: 65536,
                bytes: "fe00010000",
                exp_error: None,
            },
            TestCase {
                name: "four byte high",
                value: 4294967295,
                bytes: "feffffffff",
                exp_error: None,
            },
            TestCase {
                name: "eight byte low",
                value: 4294967296,
                bytes: "ff0000000100000000",
                exp_error: None,
            },
            TestCase {
                name: "eight byte high",
                value: u64::MAX,
                bytes: "ffffffffffffffffff",
                exp_error: None,
            },
            TestCase {
                name: "two byte not canonical",
                value: 0,
                bytes: "fd00fc",
                exp_error: Some("read_bigsize: v < 0xfd"),
            },
            TestCase {
                name: "four byte not canonical",
                value: 0,
                bytes: "fe0000ffff",
                exp_error: Some("read_bigsize: v < 0x10000"),
            },
            TestCase {
                name: "eight byte not canonical",
                value: 0,
                bytes: "ff00000000ffffffff",
                exp_error: Some("read_bigsize: v < 0x1_0000_0000"),
            },
            TestCase {
                name: "two byte short read",
                value: 0,
                bytes: "fd00",
                exp_error: Some("read_bigsize: data is <3"),
            },
            TestCase {
                name: "four byte short read",
                value: 0,
                bytes: "feffff",
                exp_error: Some("read_bigsize: data is <5"),
            },
            TestCase {
                name: "eight byte short read",
                value: 0,
                bytes: "ffffffffff",
                exp_error: Some("read_bigsize: data is <9"),
            },
            TestCase {
                name: "one byte no read",
                value: 0,
                bytes: "",
                exp_error: Some("read_bigsize: data is empty"),
            },
            TestCase {
                name: "two byte no read",
                value: 0,
                bytes: "fd",
                exp_error: Some("read_bigsize: data is <3"),
            },
            TestCase {
                name: "four byte no read",
                value: 0,
                bytes: "fe",
                exp_error: Some("read_bigsize: data is <5"),
            },
            TestCase {
                name: "eight byte no read",
                value: 0,
                bytes: "ff",
                exp_error: Some("read_bigsize: data is <9"),
            },
        ];

        for tc in tests {
            let bytes = hex_decode(tc.bytes);

            match read_bigsize(&bytes) {
                Ok((value, remaining)) => {
                    if let Some(err) = tc.exp_error {
                        panic!(
                            "[{}] expected error '{}', got value {}",
                            tc.name, err, value
                        );
                    }

                    assert_eq!(value, tc.value, "[{}] decoded value mismatch", tc.name);

                    assert!(
                        remaining.is_empty(),
                        "[{}] remaining bytes not empty",
                        tc.name
                    );
                }

                Err(e) => {
                    let err = e.to_string();

                    match tc.exp_error {
                        Some(expected) => {
                            assert_eq!(err, expected, "[{}] error mismatch", tc.name);
                        }

                        None => {
                            panic!("[{}] unexpected error: {}", tc.name, err);
                        }
                    }
                }
            }
        }
    }

    fn make_pubkey_bytes(seed: u8) -> [u8; 33] {
        use bitcoin::secp256k1::{Secp256k1, SecretKey};
        use cln_rpc::primitives::PublicKey;
        let secp = Secp256k1::new();
        let mut bytes = [0u8; 32];
        bytes[31] = seed;
        let sk = SecretKey::from_slice(&bytes).unwrap();
        PublicKey::from_secret_key(&secp, &sk).serialize()
    }

    fn make_record(flags: u16, msg_type: u16, body: &[u8]) -> Vec<u8> {
        let len = (2 + body.len()) as u16;
        let mut out = Vec::with_capacity(12 + len as usize);
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // crc
        out.extend_from_slice(&0u32.to_be_bytes()); // timestamp
        out.extend_from_slice(&msg_type.to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn make_chan_ann_body(scid: u64, node1: [u8; 33], node2: [u8; 33]) -> Vec<u8> {
        let mut body = vec![0u8; 364];
        // 0..256 sigs, 256..258 features_len = 0, 258..290 chain_hash
        body[290..298].copy_from_slice(&scid.to_be_bytes());
        body[298..331].copy_from_slice(&node1);
        body[331..364].copy_from_slice(&node2);
        body
    }

    fn make_chan_update_body(scid: u64, direction: u8, fee_ppm: u32, delay: u16) -> Vec<u8> {
        let mut body = vec![0u8; 136];
        // 0..64 sig, 64..96 chain_hash
        body[96..104].copy_from_slice(&scid.to_be_bytes());
        body[104..108].copy_from_slice(&1_700_000_000u32.to_be_bytes()); // timestamp
        body[108] = 1; // message_flags (dont_forward=0, option_channel_htlc_max=1)
        body[109] = direction & 1; // channel_flags (disabled=0)
        body[110..112].copy_from_slice(&delay.to_be_bytes());
        body[112..120].copy_from_slice(&1_000u64.to_be_bytes()); // htlc_min
        body[120..124].copy_from_slice(&1_000u32.to_be_bytes()); // base_fee
        body[124..128].copy_from_slice(&fee_ppm.to_be_bytes());
        body[128..136].copy_from_slice(&1_000_000_000u64.to_be_bytes()); // htlc_max
        body
    }

    fn write_temp_gossip(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sling_test_{}_{}_{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn test_gossip_4104_and_4102_no_oob_or_offset_desync() {
        let n1 = make_pubkey_bytes(1);
        let n2 = make_pubkey_bytes(2);
        let n3 = make_pubkey_bytes(3);
        let priv_scid: u64 = (100 << 40) | (1 << 16);
        let pub_scid: u64 = (100 << 40) | (2 << 16);

        let mut store = vec![0u8]; // gossip_store v0

        // 4104 private channel announcement: amount_sat (8) + inner_len (2) + inner_type (2) + ann
        let priv_ann = make_chan_ann_body(priv_scid, n1, n2);
        let mut body_4104 = Vec::new();
        body_4104.extend_from_slice(&1_000_000u64.to_be_bytes());
        body_4104.extend_from_slice(&((2 + priv_ann.len()) as u16).to_be_bytes());
        body_4104.extend_from_slice(&256u16.to_be_bytes());
        body_4104.extend_from_slice(&priv_ann);
        store.extend_from_slice(&make_record(0, 4104, &body_4104));

        // 4102 private channel update: inner_len (2) + inner_type (2) + update
        let priv_upd = make_chan_update_body(priv_scid, 0, 50, 34);
        let mut body_4102 = Vec::new();
        body_4102.extend_from_slice(&((2 + priv_upd.len()) as u16).to_be_bytes());
        body_4102.extend_from_slice(&258u16.to_be_bytes());
        body_4102.extend_from_slice(&priv_upd);
        store.extend_from_slice(&make_record(0, 4102, &body_4102));

        // Immediately followed by a public channel announcement + update (would be corrupted
        // or cause OOB panic under the old `*offset += len + 10` / `len + 2` bug!)
        let pub_ann = make_chan_ann_body(pub_scid, n2, n3);
        let pub_upd = make_chan_update_body(pub_scid, 0, 75, 40);
        store.extend_from_slice(&make_record(0, 256, &pub_ann));
        store.extend_from_slice(&make_record(0, 258, &pub_upd));

        let path = write_temp_gossip("4104_4102", &store);
        let mut reader = BufReader::new(File::open(&path).unwrap());
        let mut is_start_up = true;
        let mut offset = 0;
        let mut graph = LnGraph::new();
        let mut incomplete = IncompleteChannels::new();

        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);

        let priv_dir0 = ShortChannelIdDir {
            short_channel_id: ShortChannelId::from(priv_scid),
            direction: 0,
        };
        let pub_dir0 = ShortChannelIdDir {
            short_channel_id: ShortChannelId::from(pub_scid),
            direction: 0,
        };
        assert!(graph.get_state_direction(priv_dir0).is_none());
        assert_eq!(graph.get_state_direction(pub_dir0).unwrap().fee_per_millionth, 75);
    }

    #[test]
    fn test_gossip_malformed_and_truncated_records_no_panic() {
        let n1 = make_pubkey_bytes(1);
        let n2 = make_pubkey_bytes(2);
        let valid_scid: u64 = (200 << 40) | (1 << 16);

        let mut store = vec![0u8]; // gossip_store v0

        // Record with len = 0 (previously underflowed `len - 2`)
        store.extend_from_slice(&0u16.to_be_bytes());
        store.extend_from_slice(&0u16.to_be_bytes());
        store.extend_from_slice(&0u32.to_be_bytes());
        store.extend_from_slice(&0u32.to_be_bytes());

        // Record with len = 1 (previously underflowed `len - 2`)
        store.extend_from_slice(&0u16.to_be_bytes());
        store.extend_from_slice(&1u16.to_be_bytes());
        store.extend_from_slice(&0u32.to_be_bytes());
        store.extend_from_slice(&0u32.to_be_bytes());
        store.push(0xff);

        // Truncated 256, 4104, 4102, 4103, 4106
        store.extend_from_slice(&make_record(0, 256, &[0u8; 10]));
        store.extend_from_slice(&make_record(0, 4104, &[0u8; 6]));
        store.extend_from_slice(&make_record(0, 4102, &[0u8; 2]));
        store.extend_from_slice(&make_record(0, 4103, &[0u8; 4]));
        store.extend_from_slice(&make_record(0, 4106, &[0u8; 8]));

        // 256 with invalid pubkeys (all zeros)
        let bad_ann = make_chan_ann_body(valid_scid + 1, [0u8; 33], [0u8; 33]);
        store.extend_from_slice(&make_record(0, 256, &bad_ann));

        // 4104 with bogus huge inner_len exceeding record body
        let mut bad_4104 = vec![0u8; 16];
        bad_4104[8..10].copy_from_slice(&500u16.to_be_bytes());
        store.extend_from_slice(&make_record(0, 4104, &bad_4104));

        // Valid channel at the end must still parse cleanly
        let good_ann = make_chan_ann_body(valid_scid, n1, n2);
        let good_upd = make_chan_update_body(valid_scid, 0, 123, 34);
        store.extend_from_slice(&make_record(0, 256, &good_ann));
        store.extend_from_slice(&make_record(0, 258, &good_upd));

        let path = write_temp_gossip("malformed", &store);
        let mut reader = BufReader::new(File::open(&path).unwrap());
        let mut is_start_up = true;
        let mut offset = 0;
        let mut graph = LnGraph::new();
        let mut incomplete = IncompleteChannels::new();

        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);

        let dir0 = ShortChannelIdDir {
            short_channel_id: ShortChannelId::from(valid_scid),
            direction: 0,
        };
        assert_eq!(graph.get_state_direction(dir0).unwrap().fee_per_millionth, 123);
    }

    #[test]
    fn test_gossip_partial_eof_and_14byte_record_no_infinite_loop() {
        use std::io::Write;

        let n1 = make_pubkey_bytes(1);
        let n2 = make_pubkey_bytes(2);
        let scid1: u64 = (300 << 40) | (1 << 16);
        let scid2: u64 = (300 << 40) | (2 << 16);

        let mut store = vec![0u8]; // gossip_store v0
        store.extend_from_slice(&make_record(0, 256, &make_chan_ann_body(scid1, n1, n2)));
        store.extend_from_slice(&make_record(0, 258, &make_chan_update_body(scid1, 0, 42, 34)));
        // Exact 14-byte record (12 header + 2 msg_type, empty body) — previously left unparsed
        // at EOF due to `*offset + 14 < len` and caused an infinite seek-back busy loop!
        store.extend_from_slice(&make_record(0, 9999, &[]));

        // Append ONLY the first half of the next channel announcement (partial EOF write)
        let rec2_ann = make_record(0, 256, &make_chan_ann_body(scid2, n1, n2));
        let rec2_upd = make_record(0, 258, &make_chan_update_body(scid2, 0, 88, 34));
        let split_at = 20;
        store.extend_from_slice(&rec2_ann[..split_at]);

        let path = write_temp_gossip("partial_eof", &store);
        let mut reader = BufReader::new(File::open(&path).unwrap());
        let mut is_start_up = true;
        let mut offset = 0;
        let mut graph = LnGraph::new();
        let mut incomplete = IncompleteChannels::new();

        // Must return cleanly (no infinite loop!) with scid1 parsed and partial scid2 left for later
        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap();

        let dir1 = ShortChannelIdDir {
            short_channel_id: ShortChannelId::from(scid1),
            direction: 0,
        };
        let dir2 = ShortChannelIdDir {
            short_channel_id: ShortChannelId::from(scid2),
            direction: 0,
        };
        assert_eq!(graph.get_state_direction(dir1).unwrap().fee_per_millionth, 42);
        assert!(graph.get_state_direction(dir2).is_none());

        // Now append the remaining bytes and re-read: scid2 must now parse cleanly!
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&rec2_ann[split_at..]).unwrap();
        f.write_all(&rec2_upd).unwrap();
        drop(f);

        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(graph.get_state_direction(dir2).unwrap().fee_per_millionth, 88);
    }

    #[test]
    fn test_gossip_missed_compaction_clears_stale_channels() {
        let n1 = make_pubkey_bytes(1);
        let n2 = make_pubkey_bytes(2);
        let n3 = make_pubkey_bytes(3);
        let closed_scid: u64 = (400 << 40) | (1 << 16);
        let live_scid: u64 = (400 << 40) | (2 << 16);
        let uuid_2 = [2u8; 32];
        let uuid_3 = [3u8; 32];

        // Store #1: contains both closed_scid and live_scid, ending with 4105 pointing to uuid_2
        let mut store1 = vec![0u8];
        store1.extend_from_slice(&make_record(0, 256, &make_chan_ann_body(closed_scid, n1, n2)));
        store1.extend_from_slice(&make_record(0, 258, &make_chan_update_body(closed_scid, 0, 10, 34)));
        store1.extend_from_slice(&make_record(0, 256, &make_chan_ann_body(live_scid, n2, n3)));
        store1.extend_from_slice(&make_record(0, 258, &make_chan_update_body(live_scid, 0, 20, 34)));
        let mut ended_body = Vec::new();
        ended_body.extend_from_slice(&100u64.to_be_bytes());
        ended_body.extend_from_slice(&uuid_2);
        store1.extend_from_slice(&make_record(0, 4105, &ended_body));

        // Store #3 (compacted after closed_scid was deleted in store #2):
        // starts with uuid_3 (!= uuid_2) and ONLY contains live_scid (no delete_chan for closed_scid)
        let mut store3 = vec![0u8];
        store3.extend_from_slice(&make_record(0, 4107, &uuid_3));
        store3.extend_from_slice(&make_record(0, 256, &make_chan_ann_body(live_scid, n2, n3)));
        store3.extend_from_slice(&make_record(0, 258, &make_chan_update_body(live_scid, 0, 25, 34)));

        let path1 = write_temp_gossip("compact1", &store1);
        let path3 = write_temp_gossip("compact3", &store3);

        let mut graph = LnGraph::new();
        let mut incomplete = IncompleteChannels::new();
        let mut is_start_up = true;
        let mut offset = 0;

        // Read store #1 -> returns store_hint for uuid_2
        let mut reader1 = BufReader::new(File::open(&path1).unwrap());
        let hint = read_gossip_file(
            &mut is_start_up,
            &mut reader1,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap()
        .unwrap();
        assert!(!hint.missed_compaction);
        assert_eq!(hint.uuid, uuid_2);
        assert!(
            graph
                .get_state_direction(ShortChannelIdDir {
                    short_channel_id: ShortChannelId::from(closed_scid),
                    direction: 0,
                })
                .is_some()
        );

        // Open store #3 with hint for uuid_2 -> detects missed_compaction!
        let mut reader3 = BufReader::new(File::open(&path3).unwrap());
        is_start_up = true;
        offset = 0;
        let missed = read_gossip_file(
            &mut is_start_up,
            &mut reader3,
            &mut graph,
            &mut incomplete,
            &mut offset,
            Some(hint),
        )
        .unwrap()
        .unwrap();
        assert!(missed.missed_compaction);

        // Reopen store #3 from offset 0 with reset_graph (graph.clear() + incomplete.clear())
        graph.clear();
        incomplete.clear();
        let mut reader3_fresh = BufReader::new(File::open(&path3).unwrap());
        is_start_up = true;
        offset = 0;
        let next = read_gossip_file(
            &mut is_start_up,
            &mut reader3_fresh,
            &mut graph,
            &mut incomplete,
            &mut offset,
            None,
        )
        .unwrap();
        assert!(next.is_none());

        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path3);

        // closed_scid must be gone, live_scid must have its updated fee (25 ppm)!
        assert!(
            graph
                .get_state_direction(ShortChannelIdDir {
                    short_channel_id: ShortChannelId::from(closed_scid),
                    direction: 0,
                })
                .is_none()
        );
        assert_eq!(
            graph
                .get_state_direction(ShortChannelIdDir {
                    short_channel_id: ShortChannelId::from(live_scid),
                    direction: 0,
                })
                .unwrap()
                .fee_per_millionth,
            25
        );
    }
}
