#!/bin/bash

# Test script to reproduce fg panic
echo "Testing fg command panic..."

# Build the project
cargo build --release

# Start dsh and test fg command
echo "Starting dsh..."
echo "sleep 10 &" | ./target/release/dsh
echo "fg" | ./target/release/dsh

echo "Test completed."
