# Copied by dx into the generated app module (Dioxus.toml [android] proguard_rules) and read by
# R8 in a signed release build (NFR-R1, #880). The renderer's Kotlin and Ember's Activity are
# called from Rust over JNI by name, which R8 cannot see, and compose-rust ships no keep rules for
# them. Until it does, R8 leaves every class as it is: the APK is larger, but nothing JNI looks up
# is removed or renamed.
-dontshrink
-dontoptimize
-dontobfuscate

# The first release-signed build (publish-release run 43874185801) stopped in R8 on
# "Missing class javax.lang.model.element.Modifier", referenced from errorprone's compile-time
# annotations (com.google.errorprone.annotations.IncompatibleModifiers and one more) that a
# dependency carries. javax.lang.model exists only in the JDK's annotation processing API, never
# on Android, and nothing calls those annotations at run time, so R8 may ignore the reference.
-dontwarn javax.lang.model.**
-dontwarn com.google.errorprone.annotations.**
