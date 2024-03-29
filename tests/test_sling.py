#!/usr/bin/python

import logging
import time

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, sync_blockheight, wait_for
from util import get_plugin  # noqa: F401


def test_basic(node_factory, get_plugin):  # noqa: F811
    node = node_factory.get_node(options={"plugin": get_plugin})
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
            "sling-refresh-graph-interval": 599,
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
    assert configs["sling-refresh-graph-interval"]["value_int"] == 599
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
        (
            r"sling-refresh-aliasmap-interval must be "
            r"greater than or equal to 1"
        )
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-refresh-graph-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (r"sling-refresh-graph-interval must be " r"greater than or equal to 1")
    )

    node = node_factory.get_node(
        options={
            "plugin": get_plugin,
            "sling-reset-liquidity-interval": 0,
        }
    )
    assert node.daemon.is_in_log(
        (
            r"sling-reset-liquidity-interval must be "
            r"greater than or equal to 1"
        )
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
    assert node.daemon.is_in_log(
        r"sling-timeoutpay must be greater than or equal to 1"
    )

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
        opts={
            "plugin": get_plugin,
            "sling-maxhops": 2,
            "sling-refresh-graph-interval": 1,
        },
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

    # wait for plugin gossip refresh
    time.sleep(2)

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl2,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    wait_for(
        lambda: l1.rpc.listpeerchannels(l2.info["id"])["channels"][0][
            "to_us_msat"
        ]
        >= 400_000_000
    )
    wait_for(
        lambda: l1.rpc.listpeerchannels(l2.info["id"])["channels"][0][
            "to_us_msat"
        ]
        <= 600_000_000
    )


def test_pull_and_push(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2, l3 = node_factory.get_nodes(
        3, opts={"plugin": get_plugin, "sling-refresh-graph-interval": 1}
    )
    l1.fundwallet(10_000_000)
    l2.fundwallet(10_000_000)
    l3.fundwallet(10_000_000)
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    l2.rpc.connect(l3.info["id"], "localhost", l3.port)
    l3.rpc.connect(l1.info["id"], "localhost", l1.port)
    l1.rpc.fundchannel(l2.info["id"], 1_000_000, mindepth=1, announce=True)
    l2.rpc.fundchannel(l3.info["id"], 1_000_000, mindepth=1, announce=True)
    l3.rpc.fundchannel(l1.info["id"], 1_000_000, mindepth=1, announce=True)

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2, l3])

    cl1 = l1.rpc.listpeerchannels(l2.info["id"])["channels"][0][
        "short_channel_id"
    ]
    cl2 = l2.rpc.listpeerchannels(l3.info["id"])["channels"][0][
        "short_channel_id"
    ]
    cl3 = l3.rpc.listpeerchannels(l1.info["id"])["channels"][0][
        "short_channel_id"
    ]

    for n in [l1, l2, l3]:
        for scid in [cl1, cl2, cl3]:
            n.wait_channel_active(scid)

    # wait for plugin gossip refresh
    time.sleep(2)

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl3,
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
        lambda: only_one(l1.rpc.listpeerchannels(l3.info["id"])["channels"])[
            "to_us_msat"
        ]
        >= 100_000_000
    )
    wait_for(
        lambda: only_one(l1.rpc.listpeerchannels(l3.info["id"])["channels"])[
            "to_us_msat"
        ]
        <= 300_000_000
    )

    l1.rpc.call("sling-deletejob", ["all"])
    l1.rpc.call(
        "sling-job",
        {
            "scid": cl3,
            "direction": "push",
            "target": 1,
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 0,
            "depleteuptopercent": 0,
        },
    )
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    wait_for(
        lambda: only_one(l1.rpc.listpeerchannels(l3.info["id"])["channels"])[
            "to_us_msat"
        ]
        <= 120_000_000
    )


def test_stats(node_factory, bitcoind, get_plugin):  # noqa: F811
    LOGGER = logging.getLogger(__name__)
    l1, l2 = node_factory.get_nodes(
        2,
        opts={
            "plugin": get_plugin,
            "sling-maxhops": 2,
            "sling-refresh-graph-interval": 1,
            "sling-stats-delete-successes-age": 0,
            "sling-stats-delete-failures-age": 0,
        },
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

    # wait for plugin gossip refresh
    time.sleep(2)

    l1.rpc.call(
        "sling-job",
        {
            "scid": cl2,
            "direction": "pull",
            "amount": 100_000,
            "maxppm": 1000,
            "outppm": 1000,
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
        == stats_chan["successes_in_time_window"]["top_5_channel_partners"][0][
            "scid"
        ]
    )


def test_private_channel_receive(node_factory, bitcoind, get_plugin):  # noqa: F811
    l1, l2 = node_factory.get_nodes(
        2, opts={"plugin": get_plugin, "sling-refresh-graph-interval": 1}
    )
    l1.fundwallet(10_000_000)
    # setup 2 nodes with 1 private and 1 public channel
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    scid_pub, _ = l1.fundchannel(l2, 1_000_000)
    bitcoind.generate_block(1)
    sync_blockheight(bitcoind, [l1, l2])
    scid_priv, _ = l1.fundchannel(
        l2, 1_000_000, announce_channel=False, push_msat=500_000_000
    )

    bitcoind.generate_block(6)
    sync_blockheight(bitcoind, [l1, l2])

    l1.wait_channel_active(scid_pub)
    l1.wait_local_channel_active(scid_priv)
    # craft route with a private channel where source peer
    # is not the same as sendpay caller
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
    l1.daemon.wait_for_log(r"Added 2 private channels")
    l1.daemon.wait_for_log(r"Added 2 public channels")
    l1.rpc.call("sling-go", [])
    l1.daemon.wait_for_log(r"already balanced. Taking a break")
    assert l1.daemon.is_in_log(r"Rebalance SUCCESSFULL after")

    assert not l2.daemon.is_in_log(
        r"No peer channel with scid={}".format(scid_priv)
    )
