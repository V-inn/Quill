// Four versions that are one decision, not four. AGP 8.9's maximum API level is
// 35, so compiling against 36 needs 8.10 or newer; Kotlin's own compatibility
// table caps KGP 2.2.20 at AGP 8.11.1, and caps every 2.1.x at AGP 8.7.2 --
// which is below anything that can reach SDK 36, so 2.1 was never an option.
// Gradle 8.13 is AGP 8.11's documented minimum. Changing any one of these
// without checking the other three is how this ends up in an unsupported
// combination that only fails at some later, unrelated moment.
plugins {
    id("com.android.application") version "8.11.1" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20" apply false
    // From Kotlin 2.0 the Compose compiler ships with Kotlin itself, as this
    // plugin, and its version must equal the Kotlin version above. It replaces
    // the composeOptions/kotlinCompilerExtensionVersion pairing that used to
    // live in app/build.gradle.kts -- there is no separate extension version
    // left to drift out of step.
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.20" apply false
}
