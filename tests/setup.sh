#!/bin/bash
set -x
# Get the directory of the script
script_dir=$(dirname -- "$(readlink -f -- "$0")")

cargo_toml_path="$script_dir/../Cargo.toml"

# Use grep and awk to extract the version
version=$(awk -F'=' '/^\[package\]/ { in_package = 1 } in_package && /version/ { gsub(/[" ]/, "", $2); print $2; exit }' "$cargo_toml_path")

get_architecture() {
    machine=$(uname -m)

    case $machine in
        x86_64)
            echo 'x86_64-linux-gnu'
            ;;
        armv7l)
            echo 'armv7-linux-gnueabihf'
            ;;
        aarch64)
            echo 'aarch64-linux-gnu'
            ;;
        *)
            echo "No self-compiled binary found and unsupported release-architecture: $machine" >&2
            exit 1
            ;;
    esac
}
architecture=$(get_architecture)

github_url="https://github.com/daywalker90/sling/releases/download/v$version/sling-v$version-$architecture.tar.gz"


# Download the file using curl
if ! curl -L "$github_url" -o "$script_dir/sling-v$version-$architecture.tar.gz"; then
    echo "Error downloading the file from $github_url" >&2
    exit 1
fi

# Extract the contents using tar
if ! tar -xzvf "$script_dir/sling-v$version-$architecture.tar.gz" -C "$script_dir"; then
    echo "Error extracting the contents of sling-v$version-$architecture.tar.gz" >&2
    exit 1
fi