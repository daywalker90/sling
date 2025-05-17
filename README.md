<table border="0">
  <tr>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/latest_v24.08.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/latest_v24.08.yml/badge.svg?branch=main">
      </a>
    </td>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/main_v24.08.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/main_v24.08.yml/badge.svg?branch=main">
      </a>
    </td>
  </tr>
  <tr>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/latest_v24.11.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/latest_v24.11.yml/badge.svg?branch=main">
      </a>
    </td>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/main_v24.11.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/main_v24.11.yml/badge.svg?branch=main">
      </a>
    </td>
  </tr>
  <tr>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/latest_v25.02.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/latest_v25.02.yml/badge.svg?branch=main">
      </a>
    </td>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/main_v25.02.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/main_v25.02.yml/badge.svg?branch=main">
      </a>
    </td>
  </tr>
  <tr>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/latest_v25.05.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/latest_v25.05.yml/badge.svg?branch=main">
      </a>
    </td>
    <td>
      <a href="https://github.com/daywalker90/sling/actions/workflows/main_v25.05.yml">
        <img src="https://github.com/daywalker90/sling/actions/workflows/main_v25.05.yml/badge.svg?branch=main">
      </a>
    </td>
  </tr>
</table>

# sling
A core lightning plugin to automatically rebalance multiple channels.

* [Installation](#installation)
* [Building](#building)
* [Documentation](#documentation)
* [How to set options](#how-to-set-options)
* [Options](#options)
* [Feedback](#feedback)
* [Thanks](#thanks)

# Installation
For general plugin installation instructions see the plugins repo [README.md](https://github.com/lightningd/plugins/blob/master/README.md#Installation)

Release binaries for
* x86_64-linux
* armv7-linux (Raspberry Pi 32bit)
* aarch64-linux (Raspberry Pi 64bit)

can be found on the [release](https://github.com/daywalker90/sling/releases) page. If you are unsure about your architecture you can run ``uname -m``.

They require ``glibc>=2.31``, which you can check with ``ldd --version``.


# Building
You can build the plugin yourself instead of using the release binaries.
First clone the repo:

``git clone https://github.com/daywalker90/sling.git``

Install a recent rust version ([rustup](https://rustup.rs/) is recommended) and ``cd`` into the ``sling`` folder, then:

``cargo build --release``

After that the binary will be here: ``target/release/sling``

Note: Release binaries are built using ``cross`` and the ``optimized`` profile.

# Documentation

Liquidity beliefs don't get saved if you run `lightning-cli plugin start /path/to/sling` and sling is already running! Use `lightning-cli plugin stop sling` first.

## Methods

* **sling-version**
    * Print the version of the plugin.
* **sling-job**
    * Adds a rebalancing job for a channel. See [Pull sats into a channel](#pull-sats-into-a-channel) and [Push sats out of a channel](#push-sats-out-of-a-channel) for details.
* **sling-jobsettings** [*scid*]
    * List all or a specific job(s) with their settings. The settings will be from the internal representation which can differ from what you input.
    * ***scid*** Optional ShortChannelId of a job.
* **sling-go** [*scid*]
    * Start all jobs that are not already running, or just the job specified by a ShortChannelId *scid*.
    * ***scid*** Optional ShortChannelId of a job.
* **sling-stop** [*scid*]
    * Gracefully stop all running jobs or the job specified by a ShortChannelId, jobs take up to ``sling-timeoutpay`` to actually stop.
    * ***scid*** Optional ShortChannelId of a job.
* **sling-once**
    * Immediately start a one time rebalance. See [One time rebalance with exact amount](#one-time-rebalance-with-exact-amount) for details.
* **sling-stats** [*scid*] [*json*]
    * With no arguments this shows a human readable status overview for all jobs.
    * ***scid*** Optional ShortChannelId of a job to get more detailed statistics.
    * ***json*** Optional boolean, `true` outputs json.
* **sling-deletejob** *job*
    * Gracefully stops and removes *job*.
    * ***job*** Either a ShortChannelId of a job or the keyword `all` to delete all jobs.
* **sling-except-chan** *command* [*scid*]
    * Manage channels that will be avoided for all jobs.
    * ***command*** Either `list` to list all current channel exceptions or `add` to add a ShortChannelId to the exceptions or `remove` to remove ShortChannelId previously added.
    * ***scid*** The ShortChannelId to `add` or `remove`
* **sling-except-peer** *command* [*id*]
    * Manage entire nodes that will be avoided for all jobs.
    * ***command*** Either `list` to list all current node id exceptions or `add` to add a node id to the exceptions or `remove` to remove a node id previously added.
    * ***id*** The node id to `add` or `remove`

## Pull sats into a channel
To pull sats into a channel you can add a job like this:

**sling-job** -k *scid* *direction* *amount* *maxppm* [*outppm*] [*target*] [*maxhops*] [*candidates*] [*depleteuptopercent*] [*depleteuptoamount*] [*paralleljobs*]

You can completely leave out optional (those in ``[]``) arguments, with one exception: either outppm and/or candidates must be set.
:warning:You must use the ``-k keyword=value`` format for ``sling-job``!

* **scid**: The ShortChannelId to which the sats should be pulled e.g. ``704776x2087x3``.
* **direction**: Set this to ``pull`` to pull the sats into the channel declared by ``scid``.
* **amount**: The amount in sats used per rebalance operation.
* **maxppm**: The max *effective* ppm to use for the rebalances. The effective feeppm is calculated using the amount, feeppm and the base fee.
* **outppm**: While building the list of channels to pull *from*, choose only the ones where we *effectively* charge <= ``outppm``.
* **target**: Floating point between ``0`` and ``1``. E.g.: if atleast ``0.7 * channel_capacity`` is on **our** side, the job stops rebalancing and goes into idle. Default is ``0.5``.
* **maxhops**: Maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``.
* **candidates**: Optional list of our ShortChannelId's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``.
* **depleteuptopercent**: How much % to leave the candidates with on the **local** side of the channel as a floating point between 0 and <1. Default is ``0.2``. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* **depleteuptoamount**: How many sats to leave the candidates with on the **local** side of the channel. Default is ``2000000``sats. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* **paralleljobs**: How many routes to take in parallel for this job. If you know that the route will be minimum length you should keep this at ``1``. Default is ``1``. You can set this globally, see [Options](#options).

Easy example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge 0ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 outppm=0``

Advanced example: "Pull sats to our side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when ``0.8 * capacity_of_704776x2087x3`` is on our side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and (because we omitted ``outppm``) ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=pull amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## Push sats out of a channel
To push sats out of a channel you can add a job like this:

**sling-job** -k *scid* *direction* *amount* *maxppm* [*outppm*] [*target*] [*maxhops*] [*candidates*] [*depleteuptopercent*] [*depleteuptoamount*] [*paralleljobs*]

You can completely leave out optional (those in ``[]``) arguments, with one exception: either outppm and/or candidates must be set.
:warning:You must use the ``-k keyword=value`` format for ``sling-job``!

* **scid**: The ShortChannelId from which the sats should be pushed out e.g. ``704776x2087x3``.
* **direction**: Set this to ``push`` to push the sats out of the channel declared by ``scid``.
* **amount**: The amount in sats used per rebalance operation.
* **maxppm**: The max *effective* ppm to use for the rebalances. The effective feeppm is calculated using the amount, feeppm and the base fee.
* **outppm**: While building the list of channels to push *to*, choose only the ones where we *effectively* charge >= ``outppm``.
* **target**: Floating point between ``0`` and ``1``. E.g.: if atleast ``0.7 * channel_capacity`` is on **their** side, the job stops rebalancing and goes into idle. Default is ``0.5``.
* **maxhops**: Maximum number of hops allowed in a route. A hop is a node that is not us. Default is ``8``.
* **candidates**: Optional list of our ShortChannelId's to use for rebalancing this channel. E.g.: ``'["704776x2087x5","702776x1087x2"]'`` You can still combine this with ``outppm``.
* **depleteuptopercent**: How much % to leave the candidates with on the **remote** side of the channel as a floating point between 0 and <1. Default is ``0.2``. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* **depleteuptoamount**: How many sats to leave the candidates with on the **remote** side of the channel. Default is ``2000000``sats. Also see [Depleteformula](#depleteformula). You can set this globally, see [Options](#options).
* **paralleljobs**: How many routes to take in parallel for this job. If you know that the route will be minimum length you should keep this at ``1``. Default is ``1``. You can set this globally, see [Options](#options).

Easy example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm and only using candidates where we charge >=600ppm, use defaults (see [Options](#options)) for the rest of the parameters":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 outppm=600``

Advanced example: "Push sats to their side on ``704776x2087x3`` in amounts of 100000 sats while paying max 300ppm. Idle when 0.8*capacity_of_704776x2087x3 is on their side, use max 6 hops and only these other channels i have as partners: ``704776x2087x5``, ``702776x1087x2``. Use defaults for the rest of the parameters and (because we omitted ``outppm``) ignore the ppm i charge on my candidates":

``sling-job -k scid=704776x2087x3 direction=push amount=100000 maxppm=300 target=0.8 maxhops=6 candidates='["704776x2087x5","702776x1087x2"]'``

## One time rebalance with exact amount
If you just want to rebalance one time instead of continuously use the ``sling-once`` command. It is similar to ``sling-job``. You don't need to run ``sling-go`` or ``sling-stop`` for this command and some arguments are different. It will immediately start and then stop once the ``onceamount`` is rebalanced. A plugin restart will forget about all running ``sling-once`` commands!

**sling-once** -k *scid* *direction* *amount* *maxppm* *onceamount* [*outppm*] [*maxhops*] [*candidates*] [*depleteuptopercent*] [*depleteuptoamount*] [*paralleljobs*]

You can completely leave out optional (those in ``[]``) arguments, with one exception: either outppm and/or candidates must be set.
:warning:You must use the ``-k keyword=value`` format for ``sling-once``!

These are the differences to ``sling-job``:

* **onceamount**: Total amount of sats you want to rebalance, must be a multiple of ``amount``.
* **target**: Not allowed here.

## Depleteformula
Formula is ``min(depleteuptopercent * channel_capacity, depleteuptoamount)``. If you don't set one or both, the global default will be used for one or both respectively instead. You can change the global defaults here: [Options](#options)

# How to set options
``sling`` is a dynamic plugin with dynamic options, so you can start it after CLN is already running and modify it's options after the plugin is started. You have two different methods of setting the options:

1. When starting the plugin dynamically.

* Example: ``lightning-cli -k plugin subcommand=start plugin=/path/to/sling sling-refresh-peers-interval=6``

2. Permanently saving them in the CLN config file. :warning:If you want to do this while CLN is running you must use [setconfig](https://docs.corelightning.org/reference/lightning-setconfig) instead of manually editing your config file! :warning:If you have options in the config file (either by manually editing it or by using the ``setconfig`` command) make sure the plugin will start automatically with CLN (include ``plugin=/path/to/sling`` or have a symlink to ``sling`` in your ``plugins`` folder). This is because CLN will refuse to start with config options that don't have a corresponding plugin loaded. :warning:If you edit your config file manually while CLN is running and a line changes their line number CLN will crash when you use the [setconfig](https://docs.corelightning.org/reference/lightning-setconfig) command, so better stick to ``setconfig`` only during CLN's uptime!

* Example: ``lightning-cli setconfig sling-refresh-peers-interval 6``

You can mix two methods and if you set the same option with different methods, it will pick the value from your most recently used method.

# Options
* ``sling-refresh-aliasmap-interval``: How often to refresh node aliases cache in seconds. Default is every ``3600``s
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
* ``sling-inform-layers``: Inform these layers about information gained by failed rebalances. State multiple times if you want to inform more than one layer. Defaults to ``xpay``'s ``xpay`` layer. Not a dynamic option.

# Feedback
You can report issues, feedback etc. here on github or join this telegram channel: [Telegram](https://t.me/+9UKAom1Jam9hYTY6)

# Thanks
Thank you to [cdecker](https://github.com/cdecker) for helping me get into writing a plugin with cln-plugin, the people in https://t.me/lightningd and [giovannizotta](https://github.com/giovannizotta) of the original [circular](https://github.com/giovannizotta/circular) plugin.





















