# sling
A core lightning plugin to automatically rebalance multiple channels.

* [Installation](#installation)
* [Building](#building)
* [Command overview](#command-overview)
* [Pull sats into a channel](#pull-sats-into-a-channel)
* [Push sats into a channel](#push-sats-into-a-channel)
* [How to set options](#how-to-set-options)
* [Options](#options)
* [Thanks](#thanks)

## Installation
For general plugin installation instructions see the plugins repo [README.md](https://github.com/lightningd/plugins/blob/master/README.md#Installation)

:warning:Make sure to change the option ``sling-lightning-cli`` if it is different from the default, see [Options](#options) for more info

## Building
You can build the plugin yourself instead of using the release binaries.
First clone the repo:

``git clone https://github.com/daywalker90/sling.git``

Install a recent rust version ([rustup](https://rustup.rs/) is recommended) and in the ``sling`` folder run:

``cargo build --release``

After that the binary will be here: ``target/release/sling``

## Command overview

There are currently six commands:
* ``sling-go`` start all jobs that are not already running
* ``sling-stop`` gracefully stop one or all running jobs, returns immediately but jobs take up to 2mins to actually stop
* ``sling-stats`` with no arguments this shows a status overview for all jobs. Provide a short channel id to get more detailed stats
* ``sling-job`` adds a rebalancing job for a channel
* ``sling-deletejob`` stops and removes one or all jobs. E.g.: ``sling-deletejob 704776x2087x3`` or for all ``sling-deletejob all``
* ``sling-except`` add or remove short channel ids to completely avoid. E.g. ``sling-except add 704776x2087x3`` or ``sling-except remove 704776x2087x3``

## Pull sats into a channel
To pull sats into a channel you can add a job like this:

``sling-job scid pull amount maxppm (outppm) (target) (maxhops) (candidatelist)``

You can completely leave out optional arguments but you must not skip them, instead use ``None`` to not set them.

* ``scid``: the channel to which the sats should be pulled e.g. ``704776x2087x3``
* ``pull``: this is to make it clear to pull the sats into the channel instead of push
* ``amount``: the amount in sats used in the rebalance operations (obviously affects precision of rebalance targets)
* ``maxppm``: the max effective ppm to use for the rebalances
* ``outppm``: while building the list of channels to pull from, choose only the ones with <= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if ``0.7`` * channel_capacity is on **our** side the job stops rebalancing
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us.
* ``candidatelist``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``

Easy example:

``sling-job 704776x2087x3 pull 100000 300 0``

Advanced example:

``sling-job 704776x2087x3 pull 100000 300 None 0.8 6 '["704776x2087x5","702776x1087x2"]'``

## Push sats into a channel
To push sats out of a channel you can add a job like this:

``sling-job scid push amount maxppm (outppm) (target) (maxhops) (candidatelist)``

You can completely leave out optional arguments but you must not skip them, instead use ``None`` to not set them.

* ``scid``: the channel to push sats out of e.g. ``704776x2087x3``
* ``push``: this is to make it clear to push the sats into the channel instead of push
* ``amount``: the amount in sats used in the rebalance operations (obviously affects precision of rebalance targets)
* ``maxppm``: the max effective ppm to use for the rebalances
* ``outppm``: while building the list of channels to push into, choose only the ones with >= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if ``0.7`` * channel_capacity is on **their** side the job stops rebalancing
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us.
* ``candidatelist``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``

Easy example:

``sling-job 704776x2087x3 push 100000 300 600``

Advanced example:

``sling-job 704776x2087x3 push 100000 300 None 0.8 6 '["704776x2087x5","702776x1087x2"]'``

## How to set options
``sling`` is a dynamic plugin, so you can start it after cln is already running. You have two different methods of setting the options:

1. when starting the plugin via ``lightning-cli plugin -k subcommand=start plugin=/path/to/sling``
2. the cln config file

:warning:Warning: If you use the cln config file to set ``sling`` options make sure you include plugin=/path/to/sling or cln will not start next time!

You can mix these methods but if you set the same option with multiple of these methods the priority is 1. -> 2.

Examples:
1. ``lightning-cli -k plugin subcommand=start plugin=/path/to/sling sling-refresh-peers-interval=6``
2. just like other cln options in the config file: ``sling-refresh-peers-interval=6``

## Options
* :warning:``sling-lightning-cli`` location of your lightning-cli, since some rpc methods are not implemented by cln-rpc yet we have to call these via cli. Default is ``/usr/local/bin/lightning-cli``
* ``sling-refresh-peers-interval``: ``sling`` periodically calls listpeers every ``refresh-peers-interval`` seconds
and jobs use the data of the last call to check for balances etc. So this option could severely impact rebalancing target precision
if it's value is too high. Default is ``5``s
* ``sling-refresh-aliasmap-interval`` How often to refresh node aliases in seconds. Default is ``3600``s
* ``sling-refresh-graph-interval`` How often to refresh the graph in seconds. Default is ``600``s
* ``sling-reset-liquidity-interval`` After how many minutes to reset liquidity knowledge. Default is ``360``m
* ``sling-depleteuptopercent`` Up to what percent to pull or push sats from/to candidate channels as floating point between 0 and 1. Formula is min(``depleteuptopercent``*channel_capacity, ``depleteuptoamount``). Default is ``0.2``
* ``sling-depleteuptoamount`` Up to what amount to pull or push sats from/to candidate channels. Formula is min(``depleteuptopercent``*channel_capacity, ``depleteuptoamount``). Default is ``2000000``sats
* ``sling-max-htlc-count`` Max number of pending directional htlcs allowed in participating channels. Default is ``4``

## Thanks
Thank you to [cdecker](https://github.com/cdecker) for helping me get into writing a plugin with cln-plugin, the people in https://t.me/lightningd and [giovannizotta](https://github.com/giovannizotta) of the original [circular](https://github.com/giovannizotta/circular) plugin.





















