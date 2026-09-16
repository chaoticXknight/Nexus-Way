import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Release signing: keystore + passwords live OUTSIDE the repo in
// ~/keys/nexus-connect-keystore.properties (never committed). Back that
// folder up — losing the keystore means users must uninstall/reinstall.
val keystoreProps = Properties()
val keystorePropsFile = file(System.getProperty("user.home") + "/keys/nexus-connect-keystore.properties")
if (keystorePropsFile.exists()) {
    keystorePropsFile.inputStream().use { keystoreProps.load(it) }
}

android {
    namespace = "com.nexusway.connect"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.nexusway.connect"
        minSdk = 26
        targetSdk = 35
        versionCode = 2051
        val localTest = providers.gradleProperty("localReliabilityTest").orNull == "true"
        versionName = if (localTest) "0.2.51-local" else "0.2.51"
        buildConfigField("boolean", "LOCAL_RELIABILITY_TEST", localTest.toString())
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner" // Exercise native notifications and WebRTC routes on a real Android device.
    }

    testBuildType = if (providers.gradleProperty("callDeviceTests").orNull == "true") "release" else "debug" // Test the signed local release without replacing it with a differently signed debug app.

    signingConfigs {
        if (keystorePropsFile.exists()) {
            create("release") {
                storeFile = file(keystoreProps.getProperty("storeFile"))
                storePassword = keystoreProps.getProperty("storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = providers.gradleProperty("callDeviceTests").orNull != "true" // Instrumentation needs shared AndroidX classes that release shrinking otherwise removes.
            isShrinkResources = isMinifyEnabled // Production releases remain optimized; only explicit device-test builds retain test-visible classes.
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            testProguardFiles("test-proguard-rules.pro") // Keep Android test-runner annotation rules out of the shipped application.
            signingConfig = signingConfigs.findByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        buildConfig = true
        compose = true
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.09.03")
    implementation(composeBom)
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.6")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.6")
    implementation("androidx.work:work-runtime-ktx:2.9.1")
    // HIVE protocol: HTTP + WebSocket, Ed25519 (BouncyCastle), encrypted prefs.
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("org.bouncycastle:bcprov-jdk18on:1.78.1")
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    // Media: authed image loading + EXIF-stripping re-encode happens in-app.
    implementation("io.coil-kt:coil-compose:2.6.0")
    implementation("androidx.exifinterface:exifinterface:1.3.7")

    // WebRTC encrypts call media with DTLS-SRTP; HIVE only relays signaling metadata.
    implementation("io.github.webrtc-sdk:android:144.7559.09")

    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
    androidTestImplementation("androidx.test:runner:1.6.2") // Run isolated device regressions without changing enrolled accounts.
    androidTestImplementation("androidx.test.ext:junit:1.2.1") // Use the AndroidJUnit4 runner and activity lifecycle checks.
    androidTestImplementation("androidx.test.uiautomator:uiautomator:2.3.0") // Inspect the real phone's call UI rather than assuming it renders.
    androidTestImplementation("com.google.errorprone:error_prone_annotations:2.28.0") // Supply annotation classes referenced by the minified Android test runner.
}
