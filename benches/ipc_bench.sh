#!/bin/bash

set -e

cargo bench --bench ipc_bench -- reader &
sleep 1
cargo bench --bench ipc_bench -- writer
wait
