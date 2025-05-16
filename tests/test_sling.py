#!/usr/bin/python

import logging
import math
import os
import time

import pytest
from pyln.client import RpcError
from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, sync_blockheight, wait_for
from util import get_plugin  # noqa: F401

LOGGER = logging.getLogger(__name__)


def test_basic(node_factory, get_plugin):  # noqa: F811
    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
        }
    )
    result = node.rpc.call("sling-version")
    assert result is not None
    assert isinstance(result, dict) is True
    assert "version" in result


def test_options(node_factory, get_plugin):  # noqa: F811
    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-refresh-peers-interval": 2,
            "sling-refresh-aliasmap-interval": 3,
            "sling-refresh-gossmap-interval": 599,
            "sling-reset-liquidity-interval": 300,
            "sling-depleteuptopercent": 0.33,
            "sling-depleteuptoamount": 100000,
            "sling-maxhops": 4,
            "sling-candidates-min-age": 6,
            "sling-paralleljobs": 4,
            "sling-timeoutpay": 60,
            "sling-max-htlc-count": 5,
            "sling-stats-delete-failures-age": 0,
            "sling-stats-delete-successes-age": 0,
            "sling-stats-delete-failures-size": 0,
            "sling-stats-delete-successes-size": 0,
        }
    )
    configs = node.rpc.call("listconfigs")["configs"]
    assert configs["sling-refresh-peers-interval"]["value_int"] == 2
    assert configs["sling-refresh-aliasmap-interval"]["value_int"] == 3
    assert configs["sling-refresh-gossmap-interval"]["value_int"] == 599
    assert configs["sling-reset-liquidity-interval"]["value_int"] == 300
    assert configs["sling-depleteuptopercent"]["value_str"] == "0.33"
    assert configs["sling-depleteuptoamount"]["value_int"] == 100000
    assert configs["sling-maxhops"]["value_int"] == 4
    assert configs["sling-candidates-min-age"]["value_int"] == 6
    assert configs["sling-paralleljobs"]["value_int"] == 4
    assert configs["sling-timeoutpay"]["value_int"] == 60
    assert configs["sling-max-htlc-count"]["value_int"] == 5
    assert configs["sling-stats-delete-failures-age"]["value_int"] == 0
    assert configs["sling-stats-delete-successes-age"]["value_int"] == 0
    assert configs["sling-stats-delete-failures-size"]["value_int"] == 0
    assert configs["sling-stats-delete-successes-size"]["value_int"] == 0


def test_setconfig_options(node_factory, get_plugin):  # noqa: F811
    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
        }
    )

    node.rpc.setconfig("sling-refresh-gossmap-interval", 1)

    with pytest.raises(RpcError, match="needs to be greater than 0 and <1"):
        node.rpc.setconfig("sling-depleteuptopercent", 2)
    node.rpc.setconfig("sling-depleteuptopercent", 0.12)

    with pytest.raises(
        RpcError, match="out of range integral type conversion attempted"
    ):
        node.rpc.setconfig("sling-paralleljobs", 100_000)
    node.rpc.setconfig("sling-paralleljobs", 50)

    with pytest.raises(
        RpcError, match="needs to be a positive number and smaller than"
    ):
        node.rpc.setconfig("sling-stats-delete-successes-age", 99999999)


def test_option_errors(node_factory, get_plugin):  # noqa: F811
    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-refresh-peers-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-refresh-peers-interval must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-refresh-aliasmap-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-refresh-aliasmap-interval must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-refresh-gossmap-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-refresh-gossmap-interval must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-reset-liquidity-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-reset-liquidity-interval must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-depleteuptopercent": 1,
        }
    )
    assert node.daemon.is_in_log(
        r"sling-depleteuptopercent needs to be greater than 0 and <1"
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-depleteuptoamount": -10,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-depleteuptoamount needs to be a positive number")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-maxhops": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-maxhops must be " r"greater than or equal to 2")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-candidates-min-age": -10,
        }
    )
    assert node.daemon.is_in_log(
        r"sling-candidates-min-age needs to be a positive number"
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-paralleljobs": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-paralleljobs must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-timeoutpay": 0,
        }
    )
    assert node.daemon.is_in_log(r"sling-timeoutpay must be greater than or equal to 1")

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-max-htlc-count": 0,
        }
    )
    assert node.daemon.is_in_log(
        r"sling-max-htlc-count must be greater than or equal to 1"
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-stats-delete-failures-age": 99999999,
        }
    )
    assert node.daemon.is_in_log(
        (
            r"sling-stats-delete-failures-age needs to be "
            r"a positive number and smaller than"
        )
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-stats-delete-successes-age": 99999999,
        }
    )
    assert node.daemon.is_in_log(
        (
            r"sling-stats-delete-successes-age needs to be "
            r"a positive number and smaller than"
        )
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-stats-delete-failures-size": -10,
        }
    )
    assert node.daemon.is_in_log(
        r"sling-stats-delete-failures-size needs to be a positive number"
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-stats-delete-successes-size": -10,
        }
    )
    assert node.daemon.is_in_log(
        r"sling-stats-delete-successes-size needs to be a positive number"
    )


def test_maxhops_2(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2 = node_factory.get_nodes(
        2,
        opts=[
            {
                "plugin": get_plugin,
                "sling-maxhops": 2,
                "sling-refresh-gossmap-interval": 1,
            },
            {},
        ],
    )
    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    l1.rpc.fundchannel(l2.info["id"], 1_000_000, mindepth=1)
    l2.rpc.fundchannel(l1.info["id"], 1_000_000, mindepth=1)

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2])

    chans = l2.rpc.listpeerchannels(l1.info["id"])["channels"]
    for chan in chans:
        if chan["opener"] == "local":
            cl2 = chan["short_channel_id"]
        else:
            cl1 = chan["short_channel_id"]
    l2.wait_channel_active(cl1)
    l2.wait_channel_active(cl2)

    l1.daemon.wait_for_log(r"4 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl2,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.2,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")

    wait_for(
        lambda: next(
            channel
            for channel in l1.rpc.listpeerchannels(l2.info["id"])["channels"]
            if channel["opener"] == "remote"
        )["to_us_msat"]
        >= 200_000_000
    )
    assert (
        next(
            channel
            for channel in l1.rpc.listpeerchannels(l2.info["id"])["channels"]
            if channel["opener"] == "remote"
        )["to_us_msat"]
        == 200_000_000
    )


def test_pull_and_push(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3, l4, l5 = node_factory.get_nodes(
        5,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
            },
            {"fee-per-satoshi": 20, "fee-base": 2, "cltv-delta": 100},
            {"fee-per-satoshi": 30, "fee-base": 3, "cltv-delta": 150},
            {"fee-per-satoshi": 60, "fee-base": 6, "cltv-delta": 200},
            {},
        ],
    )
    nodes = [l1, l2, l3, l4, l5]
    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)
    l4.fundwallet(10_000_000)
    l5.fundwallet(10_000_000)
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    l2.rpc.connect(l3.info["id"], "localhost", l3.port)
    l3.rpc.connect(l4.info["id"], "localhost", l4.port)
    l4.rpc.connect(l5.info["id"], "localhost", l5.port)
    l5.rpc.connect(l1.info["id"], "localhost", l1.port)
    l1.rpc.fundchannel(l2.info["id"], 1_000_000, mindepth=1, announce=True)
    l2.rpc.fundchannel(l3.info["id"], 1_000_000, mindepth=1, announce=True)
    l3.rpc.fundchannel(
        l4.info["id"],
        1_000_000,
        mindepth=1,
        announce=True,
        push_msat=500_000_000,
    )
    l4.rpc.fundchannel(l5.info["id"], 1_000_000, mindepth=1, announce=True)
    l5.rpc.fundchannel(l1.info["id"], 1_000_000, mindepth=1, announce=True)
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, nodes)
    l3.rpc.fundchannel(
        l4.info["id"],
        1_000_000,
        mindepth=1,
        announce=True,
        push_msat=500_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, nodes)
    l3.rpc.fundchannel(
        l4.info["id"],
        1_000_000,
        mindepth=1,
        announce=True,
        push_msat=500_000_000,
    )
    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, nodes)

    cl1s = l1.rpc.listpeerchannels(l2.info["id"])["channels"]
    cl1 = cl1s[0]["short_channel_id"]
    cl2s = l2.rpc.listpeerchannels(l3.info["id"])["channels"]
    cl2 = cl2s[0]["short_channel_id"]
    cl3s = l3.rpc.listpeerchannels(l4.info["id"])["channels"]
    cl3_0 = cl3s[0]["short_channel_id"]
    cl3_1 = cl3s[1]["short_channel_id"]
    cl3_2 = cl3s[2]["short_channel_id"]
    cl4s = l4.rpc.listpeerchannels(l5.info["id"])["channels"]
    cl4 = cl4s[0]["short_channel_id"]
    cl5s = l5.rpc.listpeerchannels(l1.info["id"])["channels"]
    cl5 = cl5s[0]["short_channel_id"]

    fee_base_l2 = cl2s[0]["updates"]["local"]["fee_base_msat"]
    fee_decimal_l2 = (
        float(cl2s[0]["updates"]["local"]["fee_proportional_millionths"]) / 1_000_000.0
    )
    fee_base_l3 = cl3s[0]["updates"]["local"]["fee_base_msat"]
    fee_decimal_l3 = (
        float(cl3s[0]["updates"]["local"]["fee_proportional_millionths"]) / 1_000_000.0
    )
    fee_base_l4 = cl4s[0]["updates"]["local"]["fee_base_msat"]
    fee_decimal_l4 = (
        float(cl4s[0]["updates"]["local"]["fee_proportional_millionths"]) / 1_000_000.0
    )
    fee_base_l5 = cl5s[0]["updates"]["local"]["fee_base_msat"]
    fee_decimal_l5 = (
        float(cl5s[0]["updates"]["local"]["fee_proportional_millionths"]) / 1_000_000.0
    )

    LOGGER.info(f"cl1:{cl1}")
    LOGGER.info(f"cl5:{cl5}")

    for n in [l1, l2, l3]:
        for scid in [cl1, cl2, cl3_0, cl3_1, cl3_2, cl4, cl5]:
            n.wait_channel_active(scid)

    l1.daemon.wait_for_log(r"14 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl5,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.4,
            "paralleljobs": 3,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    wait_for(
        lambda: only_one(l1.rpc.listpeerchannels(l5.info["id"])["channels"])[
            "to_us_msat"
        ]
        >= 400_000_000
    )
    pull_pay = l1.rpc.call("listpays", {"status": "complete"})["pays"][0]
    LOGGER.info(f"{pull_pay}")
    first_fee = math.ceil(pull_pay["amount_msat"] * fee_decimal_l5) + fee_base_l5
    second_fee = (
        math.ceil((pull_pay["amount_msat"] + first_fee) * fee_decimal_l4) + fee_base_l4
    )
    third_fee = (
        math.ceil((pull_pay["amount_msat"] + first_fee + second_fee) * fee_decimal_l3)
        + fee_base_l3
    )
    fourth_fee = (
        math.ceil(
            (pull_pay["amount_msat"] + first_fee + second_fee + third_fee)
            * fee_decimal_l2
        )
        + fee_base_l2
    )
    assert (
        pull_pay["amount_sent_msat"] - pull_pay["amount_msat"]
        == first_fee + second_fee + third_fee + fourth_fee
    )

    l1.rpc.call("sling-deletejob", ["all"])
    l1.rpc.call(
        "sling-job",
        {
            "scid": cl5,
            "direction": "push",
            "target": 1,
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 0,
            "depleteuptopercent": 0,
            "paralleljobs": 3,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    wait_for(
        lambda: only_one(l1.rpc.listpeerchannels(l5.info["id"])["channels"])[
            "to_us_msat"
        ]
        <= 120_000_000
    )
    pays = l1.rpc.call("listpays", {"status": "complete"})["pays"]
    push_pay = pays[len(pays) - 1]
    LOGGER.info(f"{push_pay}")
    first_fee = math.ceil(push_pay["amount_msat"] * fee_decimal_l5) + fee_base_l5
    second_fee = (
        math.ceil((push_pay["amount_msat"] + first_fee) * fee_decimal_l4) + fee_base_l4
    )
    third_fee = (
        math.ceil((push_pay["amount_msat"] + first_fee + second_fee) * fee_decimal_l3)
        + fee_base_l3
    )
    fourth_fee = (
        math.ceil(
            (push_pay["amount_msat"] + first_fee + second_fee + third_fee)
            * fee_decimal_l2
        )
        + fee_base_l2
    )
    assert (
        push_pay["amount_sent_msat"] - push_pay["amount_msat"]
        == first_fee + second_fee + third_fee + fourth_fee
    )


def test_stats(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2 = node_factory.get_nodes(
        2,
        opts=[
            {
                "plugin": get_plugin,
                "sling-maxhops": 2,
                "sling-refresh-gossmap-interval": 1,
                "sling-stats-delete-successes-age": 0,
                "sling-stats-delete-failures-age": 0,
            },
            {},
        ],
    )
    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    l1.rpc.fundchannel(l2.info["id"], 1_000_000, mindepth=1)
    l2.rpc.fundchannel(l1.info["id"], 1_000_000, mindepth=1)

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2])

    chans = l2.rpc.listpeerchannels(l1.info["id"])["channels"]
    for chan in chans:
        if chan["opener"] == "local":
            cl2 = chan["short_channel_id"]
        else:
            cl1 = chan["short_channel_id"]
    l2.wait_channel_active(cl1)
    l2.wait_channel_active(cl2)

    l1.daemon.wait_for_log(r"4 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl2,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.2,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")

    stats_summary = l1.rpc.call("sling-stats", [])
    LOGGER.info(stats_summary)
    assert cl2 in stats_summary["result"]

    stats_chan = l1.rpc.call("sling-stats", [cl2])
    LOGGER.info(stats_chan)
    assert (
        cl1
        == stats_chan["successes_in_time_window"]["top_5_channel_partners"][0]["scid"]
    )
    assert (
        stats_chan["successes_in_time_window"]["top_5_channel_partners"][0]["sats"]
        == 200_000
    )
    assert stats_chan["successes_in_time_window"]["total_amount_sats"] == 200_000


def test_private_channel_receive(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2 = node_factory.get_nodes(
        2,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
            },
            {},
        ],
    )
    l1.fundwallet(10_000_000)
    # setup 2 nodes with 1 private and 1 public channel
    l1.rpc.fundchannel(l2.info["id"] + "@localhost:" + str(l2.port), 1_000_000)
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])
    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
        push_msat=500_000_000,
    )

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2])

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 2)
    for chan in l1.rpc.listpeerchannels()["channels"]:
        if chan["private"]:
            scid_priv = chan["short_channel_id"]
        else:
            scid_pub = chan["short_channel_id"]

    l1.wait_channel_active(scid_pub)

    l1.daemon.wait_for_log(r"2 private channels")
    l1.daemon.wait_for_log(r"2 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_priv,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.7,
        },
    )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    assert l1.daemon.is_in_log(r"Rebalance SUCCESSFULL after")

    assert not l2.daemon.is_in_log(r"No peer channel with scid={}".format(scid_priv))


def test_private_channel_node(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3 = node_factory.get_nodes(
        3,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
            },
            {},
            {},
        ],
    )
    l1.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)

    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])

    l1.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        1_000_000,
        announce=False,
        push_msat=900_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l3])

    l3.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2, l3])

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 2)
    for chan in l1.rpc.listpeerchannels()["channels"]:
        if chan["peer_id"] == l2.info["id"]:
            scid_l2 = chan["short_channel_id"]
        else:
            scid_l3 = chan["short_channel_id"]

    l1.daemon.wait_for_log(r"4 private channels")
    l1.daemon.wait_for_log(r"2 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l3,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.3,
        },
    )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.daemon.wait_for_log(r"already balanced. Taking a break")

    assert not l3.daemon.is_in_log(r"No peer channel with scid={}".format(scid_l2))

    l1.rpc.call("sling-deletejob", [scid_l3])

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l2,
            "direction": "push",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 0,
            "target": 0.3,
            "depleteuptoamount": 0,
        },
    )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.daemon.wait_for_log(r"already balanced. Taking a break")

    assert not l3.daemon.is_in_log(r"No peer channel with scid={}".format(scid_l2))


def test_private_candidates(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3 = node_factory.get_nodes(
        3,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
            },
            {},
            {},
        ],
    )

    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)

    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])

    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])

    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])

    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])

    l1.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        1_000_000,
        announce=False,
        push_msat=900_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l3])

    l3.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    l2.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l2.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2, l3])

    l1.daemon.wait_for_log(r"10 private channels")
    l1.daemon.wait_for_log(r"4 public channels")

    l1_l2_chans = []
    l1_l2_chan_aliases = []

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 5)
    for chan in l1.rpc.listpeerchannels()["channels"]:
        if chan["peer_id"] == l2.info["id"]:
            l1_l2_chans.append(chan["short_channel_id"])
            l1_l2_chan_aliases.append(chan["alias"]["local"])
        else:
            scid_l1_l3 = chan["short_channel_id"]

    wait_for(lambda: len(l2.rpc.listpeerchannels(l3.info["id"])["channels"]) == 2)
    l2_l3_scid = l2.rpc.listpeerchannels(l3.info["id"])["channels"][0][
        "short_channel_id"
    ]

    assert len(l1_l2_chans) == 4
    assert len(l1_l2_chan_aliases) == 4

    l1.rpc.call("sling-except-chan", ["add", l2_l3_scid])

    with pytest.raises(RpcError, match="You can't except your own channels"):
        l1.rpc.call("sling-except-chan", ["add", l1_l2_chans[3]])

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l1_l3,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.4,
            "candidates": [l1_l2_chans[0]],
        },
    )
    with pytest.raises(RpcError, match=f"candidate {scid_l1_l3} has a pull-job"):
        l1.rpc.call(
            "sling-job",
            {
                "scid": l1_l2_chans[1],
                "direction": "pull",
                "amount": 100_000,
                "maxppm": 1000,
                "outppm": 1000,
                "target": 0.4,
                "candidates": [scid_l1_l3],
            },
        )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    assert l1.daemon.is_in_log(
        f"exclude_pull_chans: {scid_l1_l3}, {l2_l3_scid}"
    ) or l1.daemon.is_in_log(f"exclude_pull_chans: {l2_l3_scid}, {scid_l1_l3}")
    assert l1.daemon.is_in_log(f"exclude_push_chans: {l2_l3_scid}")

    wait_for(
        lambda: len(
            l1.rpc.call("sling-stats", [scid_l1_l3])["successes_in_time_window"]
        )
        is not None
    )
    stats = l1.rpc.call("sling-stats", [scid_l1_l3])["successes_in_time_window"]
    for chan_partner in stats["top_5_channel_partners"]:
        assert chan_partner["scid"] == l1_l2_chan_aliases[0]


def test_once(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3 = node_factory.get_nodes(
        3,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
                "sling-depleteuptopercent": 0.0,
            },
            {},
            {},
        ],
    )
    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)
    l1.rpc.fundchannel(l2.info["id"] + "@localhost:" + str(l2.port), 1_000_000)
    l3.rpc.fundchannel(l1.info["id"] + "@localhost:" + str(l1.port), 1_000_000)
    l2.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2, l3])
    l2.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2, l3])
    l2.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2, l3])
    l2.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        2_000_000,
        push_msat=1_000_000_000,
    )
    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2, l3])

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 2)
    l1.daemon.wait_for_log(r"12 public channels")
    chans = l1.rpc.listpeerchannels()["channels"]
    for chan in chans:
        if chan["spendable_msat"] == 0:
            scid_l3 = chan["short_channel_id"]
        elif chan["receivable_msat"] == 0:
            _scid_l2 = chan["short_channel_id"]

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l3,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.1,
        },
    )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    stats = l1.rpc.call("sling-stats", [scid_l3])["successes_in_time_window"]
    assert stats["total_amount_sats"] == 100_000
    stats = l1.rpc.call("sling-stats", [True])
    assert len(stats) == 1
    assert stats[0]["status"] == ["1:Balanced"]

    with pytest.raises(RpcError, match="There is already a job for that scid!"):
        l1.rpc.call(
            "sling-once",
            {
                "scid": scid_l3,
                "direction": "pull",
                "amount": 100_000,
                "maxppm": 1000,
                "outppm": 1000,
                "total_amount": 300_000,
                "paralleljobs": 3,
            },
        )

    l1.rpc.call("sling-deletejob", [scid_l3])

    stats = l1.rpc.call("sling-stats", [True])
    assert len(stats) == 0

    l1.rpc.call(
        "sling-once",
        {
            "scid": scid_l3,
            "direction": "pull",
            "amount": 25_000,
            "maxppm": 1000,
            "outppm": 1000,
            "total_amount": 300_000,
            "paralleljobs": 1,
        },
    )

    with pytest.raises(
        RpcError, match="Once-job is currently running for this channel"
    ):
        l1.rpc.call(
            "sling-job",
            {
                "scid": scid_l3,
                "direction": "pull",
                "amount": 100_000,
                "maxppm": 1000,
                "outppm": 1000,
            },
        )

    l1.rpc.call("sling-stop", [scid_l3])
    l1.daemon.wait_for_log(r"Stopping job...")
    l1.daemon.wait_for_log(r"Spawned once-job exited")

    wait_for(lambda: l1.rpc.call("sling-stats", [True])[0]["status"] == ["1:Stopped"])

    assert (
        l1.rpc.call("sling-stats", [scid_l3])["successes_in_time_window"][
            "total_amount_sats"
        ]
        == 125_000
    )

    l1.rpc.call(
        "sling-once",
        {
            "scid": scid_l3,
            "direction": "pull",
            "amount": 25_000,
            "maxppm": 1000,
            "outppm": 1000,
            "total_amount": 100_000,
            "paralleljobs": 4,
        },
    )

    start_time = time.time()
    while time.time() < start_time + 10:
        LOGGER.info(l1.rpc.call("sling-stats", [True])[0]["status"])
        if l1.rpc.call("sling-stats", [True])[0]["status"] == [
            "1:Balanced",
            "2:Balanced",
            "3:Balanced",
            "4:Balanced",
        ]:
            break
        time.sleep(1)

    wait_for(
        lambda: l1.rpc.call("sling-stats", [True])[0]["status"]
        == [
            "1:Balanced",
            "2:Balanced",
            "3:Balanced",
            "4:Balanced",
        ]
    )

    assert (
        l1.rpc.call("sling-stats", [scid_l3])["successes_in_time_window"][
            "total_amount_sats"
        ]
        == 225_000
    )

    stats = l1.rpc.call("sling-stats", [True])
    assert len(stats) == 1
    LOGGER.info(stats[0]["status"])
    assert stats[0]["status"] == [
        "1:Balanced",
        "2:Balanced",
        "3:Balanced",
        "4:Balanced",
    ]

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l3,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
            "target": 0.4,
        },
    )

    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    stats = l1.rpc.call("sling-stats", [scid_l3])["successes_in_time_window"]
    assert stats["total_amount_sats"] == 425_000


def test_gossip(node_factory, bitcoind, get_plugin):  # noqa: F811
    #       l2
    # l1 <      > l4
    #  |    l3     |
    #  -------------
    l1, l2, l3, l4 = node_factory.get_nodes(
        4,
        opts=[
            {
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
                "may_reconnect": True,
            },
            {
                "may_reconnect": True,
            },
            {
                "may_reconnect": True,
            },
            {
                "may_reconnect": True,
            },
        ],
    )
    nodes = [l1, l2, l3, l4]

    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)
    l4.fundwallet(10_000_000)
    l1.rpc.fundchannel(
        l2.info["id"] + "@localhost:" + str(l2.port),
        1_000_000,
        announce=False,
    )
    l2.rpc.fundchannel(
        l4.info["id"] + "@localhost:" + str(l4.port),
        1_000_000,
    )
    l3.rpc.fundchannel(
        l4.info["id"] + "@localhost:" + str(l4.port),
        1_000_000,
    )
    l4.rpc.fundchannel(
        l1.info["id"] + "@localhost:" + str(l1.port),
        1_000_000,
    )
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, nodes)
    l1.rpc.fundchannel(
        l3.info["id"] + "@localhost:" + str(l3.port),
        1_000_000,
        announce=False,
    )
    l4.rpc.fundchannel(
        l1.info["id"] + "@localhost:" + str(l1.port),
        1_000_000,
    )

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, nodes)

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 4)
    l4_chans = l1.rpc.listpeerchannels(l4.info["id"])["channels"]
    scid_l1_l4_1 = l4_chans[0]["short_channel_id"]
    scid_l1_l4_2 = l4_chans[1]["short_channel_id"]
    scid_l1_l2 = l1.rpc.listpeerchannels(l2.info["id"])["channels"][0][
        "short_channel_id"
    ]
    scid_l2_l4 = l2.rpc.listpeerchannels(l4.info["id"])["channels"][0][
        "short_channel_id"
    ]

    l1.daemon.wait_for_log(r"4 private channels")
    l1.daemon.wait_for_log(r"8 public channels")

    l1.rpc.close(scid_l1_l4_1)
    bitcoind.generate_block(6, wait_for_mempool=1)
    sync_blockheight(bitcoind, nodes)

    l1.daemon.wait_for_log(r"4 private channels")
    l1.daemon.wait_for_log(r"6 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l1_l2,
            "direction": "push",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 0,
            "candidates": [scid_l1_l4_1, scid_l1_l4_2],
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.rpc.call("sling-stop", [])
    assert not l1.daemon.is_in_log(f"sling: {scid_l1_l4_1}")

    sling_liquidity = os.path.join(l1.info["lightning-dir"], "sling", "liquidity.json")
    assert os.path.getsize(sling_liquidity) == 2

    l1.restart()
    l1.rpc.connect(l2.info["id"] + "@localhost:" + str(l2.port))
    l1.rpc.connect(l3.info["id"] + "@localhost:" + str(l3.port))
    l1.rpc.connect(l4.info["id"] + "@localhost:" + str(l4.port))

    assert len(l1.rpc.call("listchannels", {})["channels"]) == 6
    l1.daemon.wait_for_log(r"4 private channels")
    l1.daemon.wait_for_log(r"6 public channels")

    l2.rpc.close(scid_l2_l4)
    bitcoind.generate_block(1, wait_for_mempool=1)
    sync_blockheight(bitcoind, nodes)

    l1.daemon.wait_for_log(r"4 private channels")
    l1.daemon.wait_for_log(r"4 public channels")

    l1.rpc.call(
        "sling-job",
        {
            "scid": scid_l1_l4_2,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
        },
    )
    l1.rpc.call("sling-go", [scid_l1_l4_2])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
    l1.rpc.call("sling-stop", [])
    assert not l1.daemon.is_in_log(f"sling: {scid_l2_l4}")

    l2.rpc.close(scid_l1_l2)
    bitcoind.generate_block(1, wait_for_mempool=1)
    sync_blockheight(bitcoind, nodes)

    l1.daemon.wait_for_log(r"2 private channels")
    l1.daemon.wait_for_log(r"4 public channels")


def test_splice(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3 = node_factory.line_graph(
        3,
        fundamount=1_000_000,
        wait_for_announce=True,
        opts=[
            {
                "experimental-splicing": None,
                "plugin": get_plugin,
                "sling-refresh-gossmap-interval": 1,
                "log-level": "info",
                "log-level": "debug:plugin-sling",  # noqa: F601
            },
            {
                "experimental-splicing": None,
                "log-level": "info",
            },
            {},
        ],
    )
    l3.fundwallet(10_000_000)

    l1.daemon.wait_for_log(r"4 public channels")

    chan_id = l1.get_channel_id(l2)
    LOGGER.info(
        l1.rpc.listpeerchannels(l2.info["id"])["channels"][0]["short_channel_id"]
    )

    funds_result = l1.rpc.fundpsbt("109000sat", "slow", 166, excess_as_change=True)

    result = l1.rpc.splice_init(chan_id, 100_000, funds_result["psbt"])
    result = l1.rpc.splice_update(chan_id, result["psbt"])
    while result["commitments_secured"] is False:
        result = l1.rpc.splice_update(chan_id, result["psbt"])
        time.sleep(0.1)
    result = l1.rpc.signpsbt(result["psbt"])
    result = l1.rpc.splice_signed(chan_id, result["signed_psbt"])

    bitcoind.generate_block(1, wait_for_mempool=result["txid"])
    sync_blockheight(bitcoind, [l1, l2])

    l1.daemon.wait_for_log(r"2 public channels")

    bitcoind.generate_block(5)
    sync_blockheight(bitcoind, [l1, l2])

    l1.daemon.wait_for_log(r"4 public channels")

    open = l3.rpc.fundchannel(
        l1.info["id"] + "@localhost:" + str(l1.port),
        1_000_000,
    )
    bitcoind.generate_block(6, wait_for_mempool=open["txid"])
    sync_blockheight(bitcoind, [l1, l2, l3])

    l1.daemon.wait_for_log(r"6 public channels")

    wait_for(lambda: len(l1.rpc.listpeerchannels()["channels"]) == 2)

    chan_id = l1.rpc.listpeerchannels(l2.info["id"])["channels"][0]["short_channel_id"]
    LOGGER.info(chan_id)

    l1.rpc.call(
        "sling-job",
        {
            "scid": chan_id,
            "direction": "push",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 0,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"Rebalance SUCCESSFULL after")
