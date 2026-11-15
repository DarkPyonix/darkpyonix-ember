# Copied by dx into the generated app module (Dioxus.toml [android] proguard_rules) and read by
# R8 in a signed release build (NFR-R1, #880). The renderer's Kotlin and Ember's Activity are
# called from Rust over JNI by name, which R8 cannot see, and compose-rust ships no keep rules for
# them. Until it does, R8 leaves every class as it is: the APK is larger, but nothing JNI looks up
# is removed or renamed.
-dontshrink
-dontoptimize
-dontobfuscate
