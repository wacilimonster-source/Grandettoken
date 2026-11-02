plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "com.wacil.grandettoken.sync"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.wacil.grandettoken.sync"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        val manifestPublicKey = System.getenv("GRANDETTOKEN_ANDROID_UPDATE_PUBLIC_KEY").orEmpty()
        val apkCertSha256 = System.getenv("GRANDETTOKEN_ANDROID_CERT_SHA256").orEmpty()
        buildConfigField("String", "UPDATE_MANIFEST_PUBLIC_KEY", "\"$manifestPublicKey\"")
        buildConfigField("String", "APK_CERT_SHA256", "\"$apkCertSha256\"")
    }

    signingConfigs {
        create("release") {
            val keystorePath = System.getenv("GRANDETTOKEN_ANDROID_KEYSTORE")
            if (!keystorePath.isNullOrBlank()) {
                storeFile = file(keystorePath)
                storePassword = System.getenv("GRANDETTOKEN_ANDROID_STORE_PASSWORD")
                keyAlias = System.getenv("GRANDETTOKEN_ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("GRANDETTOKEN_ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("release")
        }
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }
}

tasks.configureEach {
    if (name.contains("Release", ignoreCase = true)) {
        doFirst {
            require(!System.getenv("GRANDETTOKEN_ANDROID_UPDATE_PUBLIC_KEY").isNullOrBlank()) {
                "GRANDETTOKEN_ANDROID_UPDATE_PUBLIC_KEY is required for release builds"
            }
            require(!System.getenv("GRANDETTOKEN_ANDROID_CERT_SHA256").isNullOrBlank()) {
                "GRANDETTOKEN_ANDROID_CERT_SHA256 is required for release builds"
            }
            require(!System.getenv("GRANDETTOKEN_ANDROID_KEYSTORE").isNullOrBlank()) {
                "GRANDETTOKEN_ANDROID_KEYSTORE is required for release builds"
            }
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.work)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.okhttp)
    implementation(libs.bcprov)
    debugImplementation(libs.androidx.compose.ui.tooling)

    testImplementation(libs.junit)
    testImplementation(kotlin("test"))
}

// 两端共用 contract/fixtures；复制到单元测试资源目录，避免手工维护第二份。
val copyContractFixture by tasks.registering(Copy::class) {
    from(rootProject.file("../contract/fixtures/sync-v1.json"))
    into(layout.projectDirectory.dir("src/test/resources"))
}
tasks.matching { it.name.startsWith("test") || it.name == "preBuild" }.configureEach {
    dependsOn(copyContractFixture)
}
