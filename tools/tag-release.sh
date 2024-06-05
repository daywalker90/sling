#!/bin/bash

# Function to check if a string matches semantic versioning pattern
is_semver() {
    if [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        return 0
    else
        return 1
    fi
}

# Function to check if a file contains a specific version
file_contains_version() {
    local version="$1"
    local file="$2"

    if grep -q "$version" "$file"; then
        return 0
    else
        return 1
    fi
}

# Function to check if there are pending changes in Git
has_pending_changes() {
    if [ -n "$(git status --porcelain)" ]; then
        return 0
    else
        return 1
    fi
}

# Main script
if [ $# -ne 1 ]; then
    echo "Usage: $0 <version>"
    exit 1
fi

version="$1"

if ! is_semver "$version"; then
    echo "Invalid semantic version: $version"
    exit 1
fi

if ! file_contains_version "$version" "CHANGELOG.md"; then
    echo "Version $version not found in CHANGELOG.md"
    exit 1
fi

# Extract version from Cargo.toml [package] section
cargo_version=$(awk -F '"' '/^\[package\]/ {p=1} p && /version/ {print $2; exit}' Cargo.toml)
coffee_version=$(grep '^[[:space:]]*version:' coffee.yml | awk '{print $2}' | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')

if [ "$cargo_version" != "$version" ]; then
    echo "Version $version does not match the version in Cargo.toml"
    exit 1
fi

if [ "$coffee_version" != "$version" ]; then
    echo "Version $version does not match the version in coffee.yml"
    exit 1
fi

# Check for pending changes
if has_pending_changes; then
    echo "There are pending changes in the repository. Please commit or stash them before tagging."
    exit 1
fi

# If the version exists in both files, tag the current commit
git tag -a "v$version" -m "Version $version"
