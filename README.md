# sling
A core lightning plugin to automatically rebalance multiple channels.

* [Installation](#installation)
* [Building](#building)
* [Command overview](#command-overview)
* [Pull sats into a channel](#pull-sats-into-a-channel)
* [Push sats out of a channel](#push-sats-out-of-a-channel)
* [Depleteformula](#depleteformula)
* [How to set options](#how-to-set-options)
* [Options](#options)
* [Feedback](#feedback)
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

* ``sling-version`` print the version of the plugin
* ``sling-go`` start all jobs that are not already running
* ``sling-stop`` gracefully stop one or all running jobs, returns immediately but jobs take up to 2mins to actually stop
* ``sling-stats`` with no arguments this shows a status overview for all jobs. Provide a short channel id to get more detailed stats
* ``sling-job`` adds a rebalancing job for a channel, you can only have one job per channel and if you add one for the same channel it gets stopped and updated inplace
* ``sling-jobsettings`` provide a channel id (or nothing for all channels) to list the currently saved settings for the job(s)
* ``sling-deletejob`` stops and removes one or all jobs. E.g.: ``sling-deletejob 704776x2087x3`` or for all ``sling-deletejob all``. Does *not* remove raw stats from disk.
* ``sling-except-chan`` add or remove short channel ids to completely avoid. E.g. ``sling-except-chan add 704776x2087x3`` or ``sling-except-chan remove 704776x2087x3`` or list all current exceptions: ``sling-except-chan list``.
* ``sling-except-peer`` same as ``sling-except-chan`` but with node public keys

## Pull sats into a channel
To pull sats into a channel you can add a job like this:

``sling-job -k scid direction amount maxppm (outppm) (target) (maxhops) (candidates) (depleteuptopercent) (depleteuptoamount)``

You can completely leave out optional (those in ``()``) arguments, with one exception: either outppm and/or candidates must be set

* ``scid``: the channel to which the sats should be pulled e.g. ``704776x2087x3``
* ``direction``: set this to ``pull`` to make it clear to pull the sats into the channel declared by ``scid``
* ``amount``: the amount in sats used in the rebalance operations (obviously affects precision of rebalance targets)
* ``maxppm``: the max effective ppm to use for the rebalances
* ``outppm``: while building the list of channels to pull *from*, choose only the ones where we charge <= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if ``0.7`` * channel_capacity is on **our** side, the job stops rebalancing and goes into idle. Default is ``0.5``
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``
* ``candidates``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``
* ``depleteuptopercent``: how much % to leave the candidates with on *their* side of the channel. Default is ``0.2``. Also see [Depleteformula](#depleteformula). You can set these globally, see [Options](#options).
* ``depleteuptoamount``: how many sats to leave the candidates with on *their* side of the channel. Default is ``2000000``sats. Also see [Depleteformula](#depleteformula). You can set these globally, see [Options](#options).

Easy example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge 0ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 outppm=0``

Advanced example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when 0.8*capacity_of_704776x2087x3 is on our side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and in this case ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## Push sats out of a channel
To push sats out of a channel you can add a job like this:

``sling-job -k scid direction amount maxppm (outppm) (target) (maxhops) (candidates) (depleteuptopercent) (depleteuptoamount)``

You can completely leave out optional (those in ``()``) arguments, with one exception: either outppm and/or candidates must be set

* ``scid``: the channel to push sats out of e.g. ``704776x2087x3``
* ``direction``: set this to ``push`` to make it clear to push the sats out of the channel declared by ``scid``
* ``amount``: the amount in sats used in the rebalance operations (obviously affects precision of rebalance targets)
* ``maxppm``: the max effective ppm to use for the rebalances
* ``outppm``: while building the list of channels to push into, choose only the ones where we charge >= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if ``0.7`` * channel_capacity is on **their** side, the job stops rebalancing and goes into idle. Default is ``0.5``
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``
* ``candidates``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``
* ``depleteuptopercent``: how much % to leave the candidates with on *our* side of the channel. Also see [Depleteformula](#depleteformula). You can set these globally, see [Options](#options).
* ``depleteuptoamount``: how many sats to leave the candidates with on *our* side of the channel. Also see [Depleteformula](#depleteformula). You can set these globally, see [Options](#options).

Easy example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge >=600ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 outppm=600``

Advanced example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when 0.8*capacity_of_704776x2087x3 is on their side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and in this case ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## Depleteformula
Formula is min(``depleteuptopercent``*channel_capacity, ``depleteuptoamount``). If you don't set one or both, the global default will be used for one or both respectively instead. You can change the global defaults here: [Options](#options)

## How to set options
``sling`` is a dynamic plugin, so you can start it after cln is already running. You have two different methods of setting the options:

1. when starting the plugin via ``lightning-cli plugin -k subcommand=start plugin=/path/to/sling``
2. the cln config file

:warning:Warning: If you use the cln config file to set ``sling`` options make sure you include plugin=/path/to/sling or have the plugin in the folder where cln automatically starts plugins from at startup otherwise cln will not start next time!

You can mix these methods but if you set the same option with multiple of these methods the priority is 1. -> 2.

Examples:
1. ``lightning-cli -k plugin subcommand=start plugin=/path/to/sling sling-refresh-peers-interval=6``
2. just like other cln options in the config file: ``sling-refresh-peers-interval=6``

## Options
* :warning:``sling-lightning-cli`` location of your lightning-cli, since some rpc methods are not implemented by cln-rpc yet we have to call these via cli. Default is ``/usr/local/bin/lightning-cli``
* ``sling-refresh-peers-interval``: ``sling`` periodically calls listpeers every ``refresh-peers-interval`` seconds
and jobs use the data of the last call to check for balances etc. So this option could severely impact rebalancing target precision
if it's value is too high. Default is ``1``s
* ``sling-refresh-aliasmap-interval`` How often to refresh node aliases in seconds. Default is ``3600``s
* ``sling-refresh-graph-interval`` How often to refresh the graph in seconds. Default is ``600``s
* ``sling-reset-liquidity-interval`` After how many minutes to reset liquidity knowledge. Default is ``360``m
* ``sling-depleteuptopercent`` Up to what percent to pull or push sats from/to candidate channels as floating point between 0 and 1. Also see [Depleteformula](#depleteformula). Default is ``0.2``
* ``sling-depleteuptoamount`` Up to what amount to pull or push sats from/to candidate channels. Also see [Depleteformula](#depleteformula). Default is ``2000000``sats
* ``sling-max-htlc-count`` Max number of pending directional htlcs allowed in participating channels. Default is ``4``

## Feedback
You can report issues, feedback etc. here on github or join this telegram channel: [Telegram](https://t.me/+9UKAom1Jam9hYTY6)

## Thanks
Thank you to [cdecker](https://github.com/cdecker) for helping me get into writing a plugin with cln-plugin, the people in https://t.me/lightningd and [giovannizotta](https://github.com/giovannizotta) of the original [circular](https://github.com/giovannizotta/circular) plugin.





















