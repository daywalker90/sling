# Changelog


## [Unreleased]

### Fixed

- restored backward compatibility to v23.11

## [1.6.0] - 2024-05-03

### Changed

- if you had the plugin with config file options start with CLN and then changed an option and only reloaded the plugin, CLN would pass stale option values to the plugin so the load priority changed to:
    1. config file options
    2. ``plugin start`` options

### Fixed

- possibly fixed another rare deadlock