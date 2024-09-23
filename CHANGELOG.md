# Changelog

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