#!/bin/sh
# Runs the visible, network-isolated mock-registry age-selection test.

set -eu

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_directory/.."

cargo test --locked \
  fallback::tests::mock_registry_selects_old_enough_version_instead_of_29_day_release \
  -- --exact --nocapture
