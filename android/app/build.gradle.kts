plugins {
    id("com.android.application")
}

android {
    namespace = "dev.axl47.tiro"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.axl47.tiro"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildTypes.getByName("release") {
        isMinifyEnabled = true
        isShrinkResources = true
        proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}
