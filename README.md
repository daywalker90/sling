[![latest release on CLN v24.02.1](https://github.com/daywalker90/sling/actions/workflows/latest_v24.02.yml/badge.svg?branch=main)](https://github.com/daywalker90/sling/actions/workflows/latest_v24.02.yml) [![latest release on CLN v23.11.2](https://github.com/daywalker90/sling/actions/workflows/latest_v23.11.yml/badge.svg?branch=main)](https://github.com/daywalker90/sling/actions/workflows/latest_v23.11.yml) [![latest release on CLN v23.08.1](https://github.com/daywalker90/sling/actions/workflows/latest_v23.08.yml/badge.svg?branch=main)](https://github.com/daywalker90/sling/actions/workflows/latest_v23.08.yml)

[![main on CLN v24.02.1](https://github.com/daywalker90/sling/actions/workflows/main_v24.02.yml/badge.svg?event=push)](https://github.com/daywalker90/sling/actions/workflows/main_v24.02.yml) [![main on CLN v23.11.2](https://github.com/daywalker90/sling/actions/workflows/main_v23.11.yml/badge.svg?event=push)](https://github.com/daywalker90/sling/actions/workflows/main_v23.11.yml) [![main on CLN v23.08.1](https://github.com/daywalker90/sling/actions/workflows/main_v23.08.yml/badge.svg?event=push)](https://github.com/daywalker90/sling/actions/workflows/main_v23.08.yml)

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

Release binaries for
* x86_64-linux
* armv7-linux (Raspberry Pi 32bit)
* aarch64-linux (Raspberry Pi 64bit)

can be found on the [release](https://github.com/daywalker90/summars/releases) page. If you are unsure about your architecture you can run ``uname -m``.

They require ``glibc>=2.31``, which you can check with ``ldd --version``.


## Building
You can build the plugin yourself instead of using the release binaries.
First clone the repo:

``git clone https://github.com/daywalker90/sling.git``

Install a recent rust version ([rustup](https://rustup.rs/) is recommended) and ``cd`` into the ``sling`` folder, then:

``cargo build --release``

After that the binary will be here: ``target/release/sling``

## Command overview

* ``sling-version`` print the version of the plugin
* ``sling-job`` adds a rebalancing job for a channel, you can only have one job per channel and if you add one for the same channel it gets stopped and updated inplace
* ``sling-jobsettings`` provide a ShortChannelId (or nothing for all channels) to list the currently saved settings for the job(s)
* ``sling-go`` start all jobs that are not already running, or the job specified by a ShortChannelId
* ``sling-stop`` gracefully stop all running jobs or the job specified by a ShortChannelId, jobs take up to ``sling-timeoutpay`` to actually stop
* ``sling-stats`` with no arguments this shows a status overview for all jobs. Provide a ShortChannelId to get more detailed stats for that specific job
* ``sling-deletejob`` gracefully stops and removes all jobs by providing the keyword ``all`` or a single job by providing a ShortChannelId. Does *not* remove raw stats from disk.
* ``sling-except-chan`` add or remove ShortChannelIds to completely avoid or alternatively list all current exceptions with keyword ``list``.
* ``sling-except-peer`` same as ``sling-except-chan`` but with node PublicKeys

## Pull sats into a channel
To pull sats into a channel you can add a job like this:

``sling-job -k scid direction amount maxppm (outppm) (target) (maxhops) (candidates) (depleteuptopercent) (depleteuptoamount) (paralleljobs)``

You can completely leave out optional (those in ``()``) arguments, with one exception: either outppm and/or candidates must be set
:warning:You must use the ``-k keyword=value`` format for ``sling-job``!

* ``scid``: the ShortChannelId to which the sats should be pulled e.g. ``704776x2087x3``
* ``direction``: set this to ``pull`` to pull the sats into the channel declared by ``scid``
* ``amount``: the amount in sats used per rebalance operation
* ``maxppm``: the max *effective* ppm to use for the rebalances
* ``outppm``: while building the list of channels to pull *from*, choose only the ones where we *effectively* charge <= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if atleast ``0.7`` * channel_capacity is on **our** side, the job stops rebalancing and goes into idle. Default is ``0.5``
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``
* ``candidates``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``
* ``depleteuptopercent``: how much % to leave the candidates with on the local side of the channel as a floating point between 0 and <1. Default is ``0.2``. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* ``depleteuptoamount``: how many sats to leave the candidates with on the local side of the channel. Default is ``2000000``sats. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* ``paralleljobs``: How many routes to take in parallel for this job. Default is ``1``. You can set this globally, see [Options](#options).

Easy example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge 0ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 outppm=0``

Advanced example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when 0.8*capacity_of_704776x2087x3 is on our side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and (because we omitted ``outppm``) ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## Push sats out of a channel
To push sats out of a channel you can add a job like this:

``sling-job -k scid direction amount maxppm (outppm) (target) (maxhops) (candidates) (depleteuptopercent) (depleteuptoamount) (paralleljobs)``

You can completely leave out optional (those in ``()``) arguments, with one exception: either outppm and/or candidates must be set
:warning:You must use the ``-k keyword=value`` format for ``sling-job``!

* ``scid``: the ShortChannelId to push sats out of e.g. ``704776x2087x3``
* ``direction``: set this to ``push`` to make it clear to push the sats out of the channel declared by ``scid``
* ``amount``: the amount in sats used per rebalance operation
* ``maxppm``: the max *effective* ppm to use for the rebalances
* ``outppm``: while building the list of channels to push into, choose only the ones where we *effectively* charge >= ``outppm``
* ``target``: floating point between ``0`` and ``1``. E.g.: if atleast ``0.7`` * channel_capacity is on **their** side, the job stops rebalancing and goes into idle. Default is ``0.5``
* ``maxhops``: maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``
* ``candidates``: a list of our scid's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``
* ``depleteuptopercent``: how much % to leave the candidates with on the remote side of the channel as a floating point between 0 and <1. Default is ``0.2``. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* ``depleteuptoamount``: how many sats to leave the candidates with on the remote side of the channel. Default is ``2000000``sats. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* ``paralleljobs``: How many routes to take in parallel for this job. Default is ``1``.  You can set this globally, see [Options](#options).

Easy example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge >=600ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 outppm=600``

Advanced example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when 0.8*capacity_of_704776x2087x3 is on their side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and (because we omitted ``outppm``) ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## Depleteformula
Formula is ``min(depleteuptopercent * channel_capacity, depleteuptoamount)``. If you don't set one or both, the global default will be used for one or both respectively instead. You can change the global defaults here: [Options](#options)

## How to set options
``sling`` is a dynamic plugin, so you can start it after cln is already running. You have two different methods of setting the options:

1. when starting the plugin. Example: ``lightning-cli -k plugin subcommand=start plugin=/path/to/sling sling-refresh-peers-interval=6``
2. in the cln config file. Example: ``sling-refresh-peers-interval=6``

:warning:Warning: If you use the cln config file to set ``sling`` options make sure you include ``plugin=/path/to/sling`` (or have the plugin in the folder where cln automatically starts plugins from at startup) otherwise cln will not start next time!

:warning:Only config files in your lightning-dir or the network dir will be read if you start the plugin dynamically after cln is already running!

You can mix these methods but if you set the same option with multiple of these methods the priority is 1. -> 2.

## Options
* ``sling-refresh-peers-interval``: ``sling`` periodically calls listpeers every ``refresh-peers-interval`` seconds
and jobs use the data of the last call to check for balances etc. So this option could severely impact rebalancing target precision
if it's value is too high. Default is ``1``s
* ``sling-refresh-aliasmap-interval``: How often to refresh node aliases in seconds. Default is ``3600``s
* ``sling-refresh-graph-interval``: How often to refresh the graph in seconds. Default is ``600``s
* ``sling-reset-liquidity-interval``: After how many minutes to reset liquidity knowledge. Default is ``360``m
* ``sling-depleteuptopercent``: Up to what percent to pull/push sats from/to candidate channels as floating point between 0 and <1. Also see [Depleteformula](#depleteformula). Default is ``0.2``
* ``sling-depleteuptoamount``: Up to what amount to pull/push sats from/to candidate channels. Also see [Depleteformula](#depleteformula). Default is ``2000000``sats
* ``sling-maxhops``: Maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``
* ``sling-candidates-min-age``: Minimum age of channels to rebalance with in blocks. Default is ``0``
* ``sling-paralleljobs``: How many routes to take in parallel for any job. Default is ``1``
* ``sling-timeoutpay``: How long we wait for a rebalance to resolve. After this we just continue with the next route. Default is ``120``s
* ``sling-max-htlc-count``: Max number of pending htlcs allowed in participating channels (softcap). Should be higher than your highest ``parraleljobs``. Default is ``5``
* ``sling-stats-delete-failures-age``: Max age of failure stats in days and also time window for sling-stats. Default is ``30`` days, use ``0`` to never delete stats based on age
* ``sling-stats-delete-successes-age``: Max age of success stats in days and also time window for sling-stats. Default is ``30`` days, use ``0`` to never delete stats based on age
* ``sling-stats-delete-failures-size``: Max number of failure stats per channel. Default is ``10000``, use ``0`` to never delete stats based on count
* ``sling-stats-delete-successes-size``: Max number of successes stats per channel. Default is ``10000``, use ``0`` to never delete stats based on count

## Feedback
You can report issues, feedback etc. here on github or join this telegram channel: [Telegram](https://t.me/+9UKAom1Jam9hYTY6)

## Thanks
Thank you to [cdecker](https://github.com/cdecker) for helping me get into writing a plugin with cln-plugin, the people in https://t.me/lightningd and [giovannizotta](https://github.com/giovannizotta) of the original [circular](https://github.com/giovannizotta/circular) plugin.





















