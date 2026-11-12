#!/usr/bin/env bash
# Usage: ./scripts/build-android.sh [--release]
#
# Builds the Ember client as an Android APK (FR-L36), the way the compose-rust samples are
# built: `dx build --platform android` generates the Gradle project and compose-rust's build
# script puts the renderer's Kotlin into it. Needs dx (dioxus-cli 0.7), the Android SDK with
# NDK 27.1.12297006, the Rust target aarch64-linux-android and a JDK no newer than 21.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

profile=debug
args=()
if [[ "${1:-}" == "--release" ]]; then
    profile=release
    args+=(--release)
fi

: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
: "${ANDROID_NDK_HOME:=$ANDROID_HOME/ndk/27.1.12297006}"
export ANDROID_HOME ANDROID_NDK_HOME

command -v dx >/dev/null || { echo "dx is not on PATH: cargo install dioxus-cli" >&2; exit 1; }
[[ -d "$ANDROID_NDK_HOME" ]] || { echo "no NDK at $ANDROID_NDK_HOME" >&2; exit 1; }
# The Android Gradle plugin's jlink step fails on a JDK newer than 21.
java_bin="${JAVA_HOME:+$JAVA_HOME/bin/}java"
java_version="$("$java_bin" -version 2>&1 | head -1 | sed -E 's/.*"([0-9]+).*/\1/')"
if [[ "$java_version" -gt 21 ]]; then
    echo "Java $java_version: point JAVA_HOME at a 17 or 21 JDK." >&2
    exit 1
fi

project="$PWD/target/dx/ember-app/$profile/android/app"
# Naming the Kotlin source root ourselves, with no package, makes compose-rust unpack the
# renderer's Kotlin there and write no Activity: Ember supplies its own (android/MainActivity.kt,
# through `android_main_activity` in Dioxus.toml), and the generated one would replace it.
export COMPOSE_RUST_ANDROID_KOTLIN_DIR="$project/app/src/main/kotlin"

dx build --platform android --target aarch64-linux-android ${args[@]+"${args[@]}"}

find "$project/app/build/outputs/apk" -name '*.apk' -print
