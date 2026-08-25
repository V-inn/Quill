plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    // Versioned in the root build file, where it sits next to the Kotlin
    // version it has to match exactly.
    id("org.jetbrains.kotlin.plugin.compose")
}

// Release signing, read from ~/.gradle/gradle.properties rather than from
// anything in this repository. The keystore is a long-lived secret: losing it
// means never shipping an update that upgrades in place under the same
// identity, and committing it would be worse.
//
// All four deliberately fall back to null. A machine without them still builds
// a release -- unsigned, and named so -- instead of failing, which keeps the
// project buildable by anyone who clones it.
val quillKeystoreFile = providers.gradleProperty("quillKeystoreFile").orNull
val quillKeystorePassword = providers.gradleProperty("quillKeystorePassword").orNull
val quillKeyAlias = providers.gradleProperty("quillKeyAlias").orNull
val quillKeyPassword = providers.gradleProperty("quillKeyPassword").orNull
val canSignRelease = listOf(
    quillKeystoreFile, quillKeystorePassword, quillKeyAlias, quillKeyPassword,
).all { it != null } && file(quillKeystoreFile!!).exists()

android {
    namespace = "com.quill.client"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.quill.client"
        minSdk = 26
        targetSdk = 36
        // Two numbers with very different lifetimes, and only one of them is
        // for people.
        //
        // `versionCode` is Play's ordering key. It can never go down, not even
        // across a deleted release: the store refuses an upload whose code is
        // not above every code it has already seen, forever, and there is no
        // way to reclaim a burnt one. So it is a plain counter, incremented by
        // one per uploaded build -- deliberately not derived from the version
        // name, a date, or a commit count, all of which can move backwards or
        // collide when a release is rebuilt.
        //
        // `versionName` is the string a person reads, and `release-apk.sh`
        // requires the tag it is publishing to match it exactly -- that check
        // is the only thing keeping a `v0.2` release from carrying a build that
        // still calls itself 0.1.
        //
        // Still 1/"0.1": nothing has been published yet. Both move together in
        // one deliberate commit when it is.
        versionCode = 1
        versionName = "0.1"
    }

    signingConfigs {
        if (canSignRelease) {
            create("release") {
                storeFile = file(quillKeystoreFile!!)
                storePassword = quillKeystorePassword
                keyAlias = quillKeyAlias
                keyPassword = quillKeyPassword
            }
        }
    }

    buildTypes {
        release {
            // Null when the key isn't configured; the build then produces
            // app-release-unsigned.apk, which is the honest outcome.
            signingConfig = signingConfigs.findByName("release")
            // Compose ships its own R8 rules, so nothing has to be authored
            // here -- but it has to actually run, or the Compose runtime is
            // carried whole. Turned on with this change, which is also the
            // first time R8 has ever run on this codebase.
            isMinifyEnabled = true
            isShrinkResources = true
        }
    }

    buildFeatures {
        compose = true
        // For BuildConfig.DEBUG: the adb-forward transport is a development
        // path and must not be reachable in a release build. See
        // MainActivity's connection loop.
        buildConfig = true
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// Was `android { kotlinOptions { jvmTarget = "17" } }`, which Kotlin 2.2
// deprecates and 2.3 removes. Must stay 17 to match compileOptions above, or
// AGP fails the build on inconsistent JVM-target compatibility -- the JDK doing
// the building (21 here) is a separate thing from the bytecode level produced.
kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.16.0")

    // Compose, for the settings screen only -- MainActivity, GearButton and
    // CursorOverlay stay plain Views and never load a Compose class, so the
    // decode path is untouched. Compose loads at the moment the video surface
    // is being torn down anyway.
    //
    // `foundation`, deliberately not `material3`: this design replaces every
    // Material default it would have supplied (the switch, the ground colour,
    // the type ramp, the elevation model), so ~1.4 MB of defaults would only
    // have been fought at every control. See ui/QuillTheme.kt for the handful
    // of things built in its place.
    //
    // 2025.06.01 is Compose 1.8.3, the runtime generation contemporaneous with
    // the Kotlin 2.2 compiler above. That pairing is the reason for the pin:
    // there is no published minimum runtime version for a given Compose
    // compiler, and a mismatch does not fail the build -- it throws
    // IncompatibleComposeRuntimeVersionException at runtime, on the one screen
    // that uses Compose. Move the BOM and the Kotlin version together.
    val composeBom = platform("androidx.compose:compose-bom:2025.06.01")
    implementation(composeBom)
    implementation("androidx.compose.runtime:runtime")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.animation:animation")
    implementation("androidx.activity:activity-compose:1.10.1")
    debugImplementation("androidx.compose.ui:ui-tooling")

    // The first tests this project has had on the Android side. Plain JUnit on
    // the JVM, deliberately: everything covered is pure arithmetic over Ints and
    // data classes, so it needs no device, no emulator and no Robolectric. The
    // logic worth testing was made reachable without Android precisely so this
    // stayed true -- see PanelGeometry.
    testImplementation("junit:junit:4.13.2")

    // androidx.appcompat was declared but never imported -- the only androidx
    // imports in the app are WindowCompat/WindowInsets* from core-ktx. Dropped
    // with this change, which claws back part of what Compose adds.
}
