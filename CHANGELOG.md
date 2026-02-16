# Changelog

## [4.2.0] - 2026-02-16

### Fixed
- gossip store reader can handle gossip store compaction for CLN v26.04+

## [4.1.3] - 2025-12-30

### Fixed
- `sling-once`: if a channel was closed during this command, `sling` would not exit the job properly and also cause `sling-stats` to hang

## [4.1.2] - 2025-11-28

### Fixed
- Don't exit if xpay layer was not created yet (should only happen in CI)

## [4.1.1] - 2025-09-10

### Fixed
- Important fix if you have more than one channel with the same peer (see [Issue#11](https://github.com/daywalker90/sling/issues/11)): They can choose on which channel to forward sats to you and might not choose the scid from ``sling``'s route. This can be counter-productive and lead to pointless back and forth on the same channel while losing sats to fees. ``sling`` now checks if the sats arrive on the correct channel and if not fails the rebalance and bans the peer for one hour at which point it will try again. Reported by @whitslack, thank you!

## [4.1.0] - 2025-08-26

### Added
- ``sling-deletejob``: add an optional boolean argument `delete_stats` which defaults to ``false``. If set to ``true`` it will delete the stats of the job(s) aswell.
- ``sling-autogo``: new option that defaults to ``false``. If set to ``true`` it will automatically start all rebalance jobs upon sling startup.

### Changed
- raised MSRV to 1.85 since both CLN and debian stable now support it


## [4.0.0] - 2025-07-28

### Removed
- Dropped support for `CLN <= v24.02`, upgrade your node!
- :warning: Make sure to comment out any removed options in your config
- ``sling-refresh-peers-interval``: Lowering it from the default 1 doesn't help much and if you must set it higher your node is probably too slow anyways
- ``sling-refresh-gossmap-interval``: Gossip reader is so fast now there is no good reason to keep it

### Added
- `sling-once`: New command to rebalance a specific amount once
- `sling-stats`: Added human readable table view for when you provide a ShortChannelId, json flag can still be set
- Sling now also periodically reads constraint information from the ``xpay`` layer in ``CLN >= v24.11``

### Changed
- Optimized gossip file reader to be ~20x faster with similar memory usage. On my system it can read a fully synced gossip file in ~110ms and periodical checks for updates are now instant: ~0ms.
- Optimized route search to be ~2x faster, on my system down from ~25ms to ~13ms.
- Make use of the new trace level logging, some debug logs are now trace
- If possible show node alias in failure log message instead of id
- ``sling-reset-liquidity-interval``: Default value increased to 360m again, if liquidity beliefs are forgotten too quickly it may result in an infinite loop of trying the very cheapest, never succeeding paths
- Slightly increased the minimum channel cost in pathfinding
- Use actual default options so they show up in CLN's ``listconfigs``
- Refactor to show method usage in CLN's ``help``
- switch from jemalloc to mimalloc for compatibility on systems using page sizes >4k

### Fixed
- You can no longer add your own channels or your own node id to exceptions, for candidate control use ``candidatelist``/``outppm``
- Jobstates are now sorted properly in the stats view

## [3.0.6] - 2025-05-03

### Fixed
- no longer require scid aliases for private channels during route calculation aswell

## [3.0.5] - 2025-05-01

### Changed
- no longer require scid aliases for private channels

## [3.0.4] - 2025-04-29

### Added

- more debug logging for candidates selection

### Changed

- only add private channels to graph if they have an alias, remove private channels without an alias
- lower log level to debug for repetitive graph refresh lines

## [3.0.3] - 2025-03-11

### Changed

- check private channels for existing aliases when adding a job

## [3.0.2] - 2025-02-24

### Fixed

- support modified cln clients with prefix in version strings

## [3.0.1] - 2025-02-23

### Fixed

- no longer panic when private channels have missing aliases or bogus entries (still not possible to rebalance these channels without the correct data from cln)

## [3.0.0] - 2024-12-10

### Added

- ``sling-inform-layers``: CLN 24.11+ will inform these layers about sling's liquidity information gained by failed rebalances. Can be stated multiple times. Defaults to ``xpay`` which will help the ``xpay`` command.

### Changed

- ``sling-reset-liquidity-interval`` defaults to 60m now and checks every 2m (previously: 360m/10m). This is to be in sync with the aging of ``xpay``'s layer

### Fixed

- ``sling-refresh-peers-interval`` was not actually dynamic
- ``sling-refresh-aliasmap-interval`` was not actually dynamic
- ``sling-refresh-gossmap-interval`` was not actually dynamic
- ``sling-reset-liquidity-interval`` was not actually dynamic

### Removed

- ``sling-utf8``, it was not used

## [2.1.1] - 2024-10-21

### Fixed
- rebalances between private channels would fail after the first if only private channels are candidates

## [2.1.0] - 2024-09-23

### Added
- nix flake (thanks to @RCasatta)
- optional boolean argument for ``sling-stats`` to set output of the summary to json, e.g. ``sling-stats true``

### Changed
- updated dependencies

## [2.0.0] - 2024-06-05

### Added

- ``sling-refresh-gossmap-interval``: How often to read ``gossip_store`` updates in seconds. Default is every ``10``s

### Changed

- ``sling`` now reads the ``gossip_store`` file directly instead of polling the whole graph via ``listchannels``. This drastically reduces CPU usage on and especially after the first read. Also, since ``sling`` is reading incremental changes to the ``gossip_store`` file, it can poll it much more often to keep a closer track of any gossip changes.
- Options code refactored. All options are now natively dynamic and there is no longer any manual reading of config files. Read the updated README section on how to set options for more information

### Removed

- ``sling-refresh-graph-interval``, replaced by ``sling-refresh-gossmap-interval``

### Fixed

- Restored backward compatibility to v23.11
- When running ``paralleljobs``>1 there were still duplicate routes taken sometimes

## [1.6.0] - 2024-05-03

### Changed

- if you had the plugin with config file options start with CLN and then changed an option and only reloaded the plugin, CLN would pass stale option values to the plugin so the load priority changed to:
    1. config file options
    2. ``plugin start`` options

### Fixed

- possibly fixed another rare deadlock
