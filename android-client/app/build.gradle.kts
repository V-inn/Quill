plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.quill.client"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.quill.client"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1"
    }

    buildTypes {
        release {
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
    }
    composeOptions {
        // Bound to Kotlin 1.9.24 exactly (see the root build.gradle.kts). They
        // are a matched pair; the build fails at configure time if they drift,
        // which is the failure mode you want. Do not bump Kotlin without
        // bumping this, and do not bump either as a side effect of some other
        // change.
        kotlinCompilerExtensionVersion = "1.5.14"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")

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
    // BOM pinned at 2024.06.00: Compose 1.7.x (BOM 2024.09+) expects the
    // Kotlin 2.0 compiler, and this project is on 1.9.24.
    val composeBom = platform("androidx.compose:compose-bom:2024.06.00")
    implementation(composeBom)
    implementation("androidx.compose.runtime:runtime")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.animation:animation")
    implementation("androidx.activity:activity-compose:1.9.0")
    debugImplementation("androidx.compose.ui:ui-tooling")

    // androidx.appcompat was declared but never imported -- the only androidx
    // imports in the app are WindowCompat/WindowInsets* from core-ktx. Dropped
    // with this change, which claws back part of what Compose adds.
}
