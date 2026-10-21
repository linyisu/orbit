plugins {
    id("com.android.application")
}

val mobileRevision = "c24a1308e1dcb47ce94d9ef8d8d05ca4d77d2aa7"
val mobileSource = providers.gradleProperty("gpuiMobileSource").orNull
    ?: error("Set -PgpuiMobileSource to the pinned gpui-mobile Git checkout")
val actualRevision = providers.exec {
    commandLine("git", "-C", mobileSource, "rev-parse", "HEAD")
}.standardOutput.asText.get().trim()
check(actualRevision == mobileRevision) { "gpui-mobile revision must be $mobileRevision" }

val prepareMobileHost by tasks.registering(Sync::class) {
    from("$mobileSource/example/android/gradle/app/src/main/java") {
        include("dev/gpui/mobile/GpuiInputActivity.java")
        include("dev/gpui/mobile/GpuiClipboard.java")
    }
    into(layout.buildDirectory.dir("generated/mobileHost"))
}

val prepareMobileLicense by tasks.registering(Sync::class) {
    from("$mobileSource/LICENSE-APACHE")
    into(layout.buildDirectory.dir("generated/mobileLicenses"))
}

android {
    namespace = "dev.orbit.mobile.probe"
    compileSdk = 35
    defaultConfig {
        applicationId = "dev.orbit.mobile.probe"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }
    sourceSets.getByName("main") {
        java.setSrcDirs(listOf(layout.buildDirectory.dir("generated/mobileHost")))
        assets.srcDir(layout.buildDirectory.dir("generated/mobileLicenses"))
    }
}

val verifyRustLibrary by tasks.registering {
    doLast {
        check(file("src/main/jniLibs/arm64-v8a/liborbit_mobile_probe.so").isFile) {
            "Build the Android Rust library before assembling the APK; see mobile/README.md"
        }
    }
}
tasks.named("preBuild") {
    dependsOn(verifyRustLibrary, prepareMobileHost, prepareMobileLicense)
}
