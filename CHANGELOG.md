# Changelog,

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